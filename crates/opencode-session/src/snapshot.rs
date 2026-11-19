use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

#[derive(Debug, Clone)]
pub struct SnapshotPatch {
    pub hash: String,
    pub files: Vec<String>,
}

pub struct Snapshot;

impl Snapshot {
    pub fn new() -> Self {
        Self
    }

    pub fn track(directory: &Path) -> Result<String> {
        let git_dir = ensure_snapshot_repo(directory)?;
        git_add_all(directory, &git_dir)?;
        let output = git_output(directory, &git_dir, &["write-tree"])?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    pub fn revert(directory: &Path, patches: Vec<SnapshotPatch>) -> Result<()> {
        Self::revert_with_git_ops(directory, patches, git_output_allow_failure, git_output)
    }

    /// Same as [`Snapshot::revert`], but allows the `ls-tree` probe to be supplied by callers.
    ///
    /// Keeping the probe injectable lets tests exercise the fail-closed branch without
    /// depending on git internals failing in a particular way.
    #[cfg(test)]
    fn revert_with_ls_tree<F>(
        directory: &Path,
        patches: Vec<SnapshotPatch>,
        ls_tree: F,
    ) -> Result<()>
    where
        F: Fn(&Path, &Path, &[&str]) -> Result<Output>,
    {
        Self::revert_with_git_ops(directory, patches, git_output_allow_failure, ls_tree)
    }

    /// Core revert with both git probes injectable so failed restorations can be
    /// tested deterministically.
    fn revert_with_git_ops<C, L>(
        directory: &Path,
        patches: Vec<SnapshotPatch>,
        checkout: C,
        ls_tree: L,
    ) -> Result<()>
    where
        C: Fn(&Path, &Path, &[&str]) -> Result<Output>,
        L: Fn(&Path, &Path, &[&str]) -> Result<Output>,
    {
        let git_dir = ensure_snapshot_repo(directory)?;
        let mut seen = HashSet::new();
        // Only a count is retained: paths and hashes must never enter an error
        // message (see the no-leak requirement in the session integrity task).
        let mut failed = 0usize;

        for patch in patches {
            for file in patch.files {
                let file_path = PathBuf::from(&file);
                let relative = relative_to_worktree(directory, &file_path);
                if relative.is_empty()
                    || !is_worktree_relative(&relative)
                    || !seen.insert(relative.clone())
                {
                    continue;
                }

                let checkout = match checkout(
                    directory,
                    &git_dir,
                    &["checkout", patch.hash.as_str(), "--", relative.as_str()],
                ) {
                    Ok(output) => output,
                    Err(_) => {
                        failed += 1;
                        continue;
                    }
                };
                if checkout.status.success() {
                    continue;
                }

                // Only a "pathspec is not in the tree" failure is eligible for deletion.
                // Other checkout failures (permission, lock, conflict, ...) must not
                // be mistaken for an absent path — they are restoration failures.
                if checkout_failure_is_missing_path(&checkout) {
                    // Confirm absence with ls-tree. If the probe itself fails, the
                    // file is counted as unrestorable rather than deleted.
                    let tree_output = match ls_tree(
                        directory,
                        &git_dir,
                        &["ls-tree", patch.hash.as_str(), "--", relative.as_str()],
                    ) {
                        Ok(output) => output,
                        Err(_) => {
                            failed += 1;
                            continue;
                        }
                    };
                    let exists_in_tree = !String::from_utf8_lossy(&tree_output.stdout)
                        .trim()
                        .is_empty();
                    if !exists_in_tree {
                        let absolute = if file_path.is_absolute() {
                            file_path
                        } else {
                            directory.join(&relative)
                        };
                        match fs::remove_file(&absolute) {
                            Ok(()) => continue,
                            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                                // Already gone; the desired end state is satisfied.
                                continue;
                            }
                            Err(_) => {
                                failed += 1;
                                continue;
                            }
                        }
                    }
                }

                failed += 1;
            }
        }

        if failed > 0 {
            anyhow::bail!("snapshot revert failed to restore {failed} file(s); paths are omitted");
        }
        Ok(())
    }

    pub fn restore(directory: &Path, snapshot: &str) -> Result<()> {
        let git_dir = ensure_snapshot_repo(directory)?;
        git_output(directory, &git_dir, &["read-tree", snapshot])?;
        git_output(directory, &git_dir, &["checkout-index", "-a", "-f"])?;
        Ok(())
    }

    pub fn diff(directory: &Path, from_hash: &str) -> Result<Vec<super::FileDiff>> {
        let git_dir = ensure_snapshot_repo(directory)?;
        git_add_all(directory, &git_dir)?;
        let output = git_output(
            directory,
            &git_dir,
            &[
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.quotepath=false",
                "diff",
                "--no-ext-diff",
                "--numstat",
                from_hash,
                "--",
                ".",
            ],
        );

        let mut diffs = Vec::new();

        if let Ok(output) = output {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if line.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = line.split('\t').collect();
                if parts.len() >= 3 {
                    let is_binary = parts[0] == "-" && parts[1] == "-";
                    let additions = if is_binary {
                        0
                    } else {
                        parts[0].parse::<u64>().unwrap_or(0)
                    };
                    let deletions = if is_binary {
                        0
                    } else {
                        parts[1].parse::<u64>().unwrap_or(0)
                    };
                    let path = parts[2].to_string();
                    diffs.push(super::FileDiff {
                        path,
                        additions,
                        deletions,
                    });
                }
            }
        }

        Ok(diffs)
    }

    /// Compute diff between two git refs (matching TS `Snapshot.diffFull`).
    ///
    /// Runs `git diff --numstat --no-renames <from> <to> -- .` to get per-file
    /// addition/deletion counts, then `git diff --name-status --no-renames <from> <to> -- .`
    /// to determine whether each file was added, deleted, or modified.
    pub fn diff_full(directory: &Path, from: &str, to: &str) -> Result<Vec<super::FileDiff>> {
        let git_dir = ensure_snapshot_repo(directory)?;
        let numstat_output = git_output(
            directory,
            &git_dir,
            &[
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.quotepath=false",
                "diff",
                "--no-ext-diff",
                "--no-renames",
                "--numstat",
                from,
                to,
                "--",
                ".",
            ],
        );

        let mut diffs = Vec::new();

        if let Ok(output) = numstat_output {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if line.is_empty() {
                    continue;
                }
                let parts: Vec<&str> = line.split('\t').collect();
                if parts.len() >= 3 {
                    let is_binary = parts[0] == "-" && parts[1] == "-";
                    let additions = if is_binary {
                        0
                    } else {
                        parts[0].parse::<u64>().unwrap_or(0)
                    };
                    let deletions = if is_binary {
                        0
                    } else {
                        parts[1].parse::<u64>().unwrap_or(0)
                    };
                    let path = parts[2].to_string();
                    diffs.push(super::FileDiff {
                        path,
                        additions,
                        deletions,
                    });
                }
            }
        }

        Ok(diffs)
    }
}

impl Default for Snapshot {
    fn default() -> Self {
        Self::new()
    }
}

fn snapshot_git_dir(directory: &Path) -> PathBuf {
    directory.join(".opencode").join("snapshot")
}

fn ensure_snapshot_repo(directory: &Path) -> Result<PathBuf> {
    let git_dir = snapshot_git_dir(directory);
    let parent = git_dir
        .parent()
        .context("snapshot git directory has no parent")?;
    fs::create_dir_all(parent)?;

    if !git_dir.join("HEAD").exists() {
        let init_output = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .current_dir(directory)
            .env("GIT_DIR", &git_dir)
            .env("GIT_WORK_TREE", directory)
            .output()
            .context("failed to run git init for snapshot repo")?;
        if !init_output.status.success() {
            anyhow::bail!(
                "failed to initialize snapshot repo: {}",
                String::from_utf8_lossy(&init_output.stderr).trim()
            );
        }

        // Keep line endings stable across platforms.
        let _ = git_output(directory, &git_dir, &["config", "core.autocrlf", "false"]);
    }

    Ok(git_dir)
}

fn git_output(directory: &Path, git_dir: &Path, args: &[&str]) -> Result<Output> {
    let output = git_output_allow_failure(directory, git_dir, args)?;

    if !output.status.success() {
        anyhow::bail!(
            "git command failed: git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(output)
}

/// Run a git command and return its raw output, including a non-zero exit status.
///
/// Only a failure to spawn the process is reported as an error. Callers decide
/// how to interpret the status and stderr, which is required to classify a
/// failed `git checkout` safely.
fn git_output_allow_failure(directory: &Path, git_dir: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .arg("--work-tree")
        .arg(directory)
        .args(args)
        .current_dir(directory)
        .output()
        .with_context(|| format!("failed to run git command: git {}", args.join(" ")))
}

/// Whether a failed `git checkout` means the pathspec is absent from the tree.
///
/// Git reports this as `pathspec '<path>' did not match any file(s) known to git`.
/// The second, looser check covers git versions/variants with slightly different
/// wording. An unrecognised failure is deliberately treated as "not absent", so
/// no file is deleted on that basis.
fn checkout_failure_is_missing_path(output: &Output) -> bool {
    let stderr = String::from_utf8_lossy(&output.stderr);
    stderr.contains("did not match any file(s) known to git")
        || (stderr.contains("pathspec") && stderr.contains("did not match"))
}

fn git_add_all(directory: &Path, git_dir: &Path) -> Result<()> {
    // Exclude the snapshot repo itself when it lives under worktree.
    for args in [
        vec!["add", "-A", "--", ".", ":(exclude).opencode/snapshot"],
        vec!["add", "-A", "--", ".", ":!/.opencode/snapshot"],
        vec!["add", "-A", "--", ".", ":!.opencode/snapshot"],
    ] {
        if git_output(directory, git_dir, &args).is_ok() {
            return Ok(());
        }
    }

    anyhow::bail!("failed to add worktree files to snapshot index with snapshot-dir exclusion")
}

fn relative_to_worktree(worktree: &Path, input: &Path) -> String {
    if input.is_absolute() {
        input
            .strip_prefix(worktree)
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default()
    } else {
        input.to_string_lossy().to_string()
    }
}

/// Whether a worktree-relative path stays inside the worktree.
///
/// `SnapshotPatch.files` is publicly constructible, so callers can supply
/// paths such as `"../outside.txt"`. Those must be excluded before any git
/// operation or deletion is attempted.
fn is_worktree_relative(relative: &str) -> bool {
    Path::new(relative)
        .components()
        .all(|component| match component {
            Component::ParentDir | Component::RootDir => false,
            #[cfg(windows)]
            Component::Prefix(_) => false,
            _ => true,
        })
}

#[cfg(test)]
mod tests {
    use super::{checkout_failure_is_missing_path, git_output, Snapshot, SnapshotPatch};
    use std::fs;
    use std::path::Path;
    use std::process::Output;
    use tempfile::tempdir;

    fn is_hex_40(s: &str) -> bool {
        s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
    }

    #[test]
    fn track_diff_restore_handles_uncommitted_worktree() {
        let temp = tempdir().expect("temp dir");
        let root = temp.path();
        let file = root.join("test_snapshot.txt");

        fs::write(&file, "test\n").expect("write initial file");
        let snapshot = Snapshot::track(root).expect("track snapshot");
        assert!(is_hex_40(&snapshot), "expected tree hash, got: {snapshot}");

        fs::write(&file, "modified\n").expect("modify file");
        let diffs = Snapshot::diff(root, &snapshot).expect("diff from snapshot");
        assert!(
            diffs.iter().any(|d| d.path == "test_snapshot.txt"),
            "expected changed file in diff"
        );

        Snapshot::restore(root, &snapshot).expect("restore snapshot");
        let restored = fs::read_to_string(&file).expect("read restored file");
        assert_eq!(restored, "test\n");
    }

    #[test]
    fn diff_full_between_two_tree_snapshots() {
        let temp = tempdir().expect("temp dir");
        let root = temp.path();
        let file = root.join("diff_full.txt");

        fs::write(&file, "v1\n").expect("write v1");
        let first = Snapshot::track(root).expect("first snapshot");
        fs::write(&file, "v2\n").expect("write v2");
        let second = Snapshot::track(root).expect("second snapshot");

        let diffs = Snapshot::diff_full(root, &first, &second).expect("diff full");
        assert!(
            diffs.iter().any(|d| d.path == "diff_full.txt"),
            "expected file change in diff_full"
        );
    }

    #[test]
    fn revert_deletes_file_absent_from_snapshot() {
        let temp = tempdir().expect("temp dir");
        let root = temp.path();
        let tracked = root.join("tracked.txt");
        fs::write(&tracked, "keep\n").expect("write tracked file");
        let hash = Snapshot::track(root).expect("track snapshot");

        // Created after the snapshot, so it is provably absent from the tree.
        let extra = root.join("extra.txt");
        fs::write(&extra, "delete me\n").expect("write extra file");
        assert!(extra.exists());

        Snapshot::revert(
            root,
            vec![SnapshotPatch {
                hash,
                files: vec![extra.to_string_lossy().to_string()],
            }],
        )
        .expect("revert");

        assert!(!extra.exists(), "file absent from snapshot must be deleted");
        assert!(tracked.exists(), "unrelated tracked file must survive");
    }

    #[test]
    fn revert_keeps_file_present_in_snapshot() {
        let temp = tempdir().expect("temp dir");
        let root = temp.path();
        let file = root.join("present.txt");
        fs::write(&file, "original\n").expect("write file");
        let hash = Snapshot::track(root).expect("track snapshot");

        fs::write(&file, "modified\n").expect("modify file");

        Snapshot::revert(
            root,
            vec![SnapshotPatch {
                hash,
                files: vec![file.to_string_lossy().to_string()],
            }],
        )
        .expect("revert");

        assert_eq!(
            fs::read_to_string(&file).expect("read file"),
            "original\n",
            "file present in snapshot must be restored, not deleted"
        );
    }

    #[test]
    fn checkout_failure_is_missing_path_only_for_pathspec_errors() {
        // Classify stderr directly: relying on how a particular git version
        // reports a directory/file conflict is environment-dependent.
        // Build a real Output via a failing process, then override only stderr.
        let mut pathspec = std::process::Command::new("git")
            .arg("--version")
            .output()
            .expect("run git for Output fixture");
        pathspec.stdout = Vec::new();
        pathspec.stderr = b"error: pathspec 'x' did not match any file(s) known to git\n".to_vec();
        assert!(checkout_failure_is_missing_path(&pathspec));

        let mut conflict: Output = std::process::Command::new("git")
            .arg("--version")
            .output()
            .expect("run git for Output fixture");
        conflict.stdout = Vec::new();
        conflict.stderr =
            b"error: Your local changes to 'x' would be overwritten by checkout\n".to_vec();
        assert!(!checkout_failure_is_missing_path(&conflict));
    }

    #[test]
    fn revert_fails_closed_when_ls_tree_fails() {
        let temp = tempdir().expect("temp dir");
        let root = temp.path();
        let tracked = root.join("tracked.txt");
        fs::write(&tracked, "keep\n").expect("write tracked file");
        let hash = Snapshot::track(root).expect("track snapshot");

        let extra = root.join("extra.txt");
        fs::write(&extra, "must survive\n").expect("write extra file");

        let result = Snapshot::revert_with_ls_tree(
            root,
            vec![SnapshotPatch {
                hash,
                files: vec![extra.to_string_lossy().to_string()],
            }],
            |directory, git_dir, args| {
                if args.first() == Some(&"ls-tree") {
                    anyhow::bail!("forced ls-tree failure");
                }
                git_output(directory, git_dir, args)
            },
        );

        assert!(result.is_err(), "ls-tree failure must be returned as Err");
        assert!(
            extra.exists(),
            "file must not be deleted when ls-tree fails"
        );
    }

    #[test]
    fn revert_does_not_delete_outside_worktree_via_parent_dir() {
        let temp = tempdir().expect("temp dir");
        let root = temp.path().join("worktree");
        fs::create_dir(&root).expect("create worktree");
        let tracked = root.join("tracked.txt");
        fs::write(&tracked, "keep\n").expect("write tracked file");
        let hash = Snapshot::track(&root).expect("track snapshot");

        // Publicly constructible patches may contain a traversal path. The file
        // lives outside the worktree and must never become a deletion target.
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, "must survive\n").expect("write outside file");

        Snapshot::revert(
            &root,
            vec![SnapshotPatch {
                hash,
                files: vec!["../outside.txt".to_string()],
            }],
        )
        .expect("revert");

        assert!(
            outside.exists(),
            "file outside worktree must not be deleted"
        );
    }

    /// 1 ファイルでも復元できなければ Err を返し、パスやハッシュを露出しないこと。
    #[test]
    fn revert_returns_err_when_any_file_cannot_be_restored() {
        let temp = tempdir().expect("temp dir");
        let root = temp.path();
        let good = root.join("good.txt");
        let bad = root.join("bad.txt");
        fs::write(&good, "v1\n").expect("write good file");
        fs::write(&bad, "v1\n").expect("write bad file");
        let hash = Snapshot::track(root).expect("track snapshot");
        fs::write(&good, "changed\n").expect("modify good file");
        fs::write(&bad, "changed\n").expect("modify bad file");

        let result = Snapshot::revert_with_git_ops(
            root,
            vec![SnapshotPatch {
                hash: hash.clone(),
                files: vec![
                    good.to_string_lossy().to_string(),
                    bad.to_string_lossy().to_string(),
                ],
            }],
            |directory: &Path, git_dir: &Path, args: &[&str]| -> anyhow::Result<Output> {
                if args.iter().any(|arg| arg.ends_with("bad.txt")) {
                    let mut output = std::process::Command::new("git")
                        .arg("not-a-git-subcommand")
                        .output()
                        .expect("produce a failed git status");
                    output.stderr = b"error: restore blocked\n".to_vec();
                    Ok(output)
                } else {
                    git_output(directory, git_dir, args)
                }
            },
            git_output,
        );

        let err = result.expect_err("a single failed file must make revert return Err");
        let rendered = err.to_string();
        assert!(
            rendered.contains("1 file"),
            "error must report failure count only: {rendered}"
        );
        assert!(!rendered.contains("good.txt"), "path must not leak");
        assert!(!rendered.contains("bad.txt"), "path must not leak");
        assert!(
            !rendered.contains(hash.as_str()),
            "snapshot hash must not leak"
        );
        assert_eq!(fs::read_to_string(&good).expect("read good"), "v1\n");
        assert_eq!(
            fs::read_to_string(&bad).expect("read bad"),
            "changed\n",
            "unrestorable file must be left untouched"
        );
    }
}

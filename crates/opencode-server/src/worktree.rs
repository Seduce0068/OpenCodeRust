use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("Not a git repository")]
    NotGitRepo,
    #[error("Git command failed: {0}")]
    GitError(String),
    #[error("Worktree not found: {0}")]
    NotFound(String),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: String,
    pub branch: String,
    pub head: String,
}

fn run_git(args: &[&str], cwd: &Path) -> Result<String, WorktreeError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| WorktreeError::GitError(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(WorktreeError::GitError(stderr.to_string()));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn is_git_repo(path: &Path) -> bool {
    path.join(".git").exists() || run_git(&["rev-parse", "--git-dir"], path).is_ok()
}

/// 現在の worktree レコードを結果へ push する共通処理。
///
/// detached worktree は `branch` 行を持たないため、branch が無い場合は
/// 従来の末尾フォールバックと同じく `"HEAD"` を採用する。path が無い
/// （推定開始前）レコードは無視する。
fn push_worktree(
    worktrees: &mut Vec<WorktreeInfo>,
    path: &Option<String>,
    branch: &Option<String>,
    head: &Option<String>,
) {
    if let Some(path) = path {
        worktrees.push(WorktreeInfo {
            path: path.clone(),
            branch: branch.clone().unwrap_or_else(|| "HEAD".to_string()),
            head: head.clone().unwrap_or_default(),
        });
    }
}

/// `git worktree list --porcelain` の出力をパースする。
///
/// 各レコードは空行で区切られ、`worktree <path>` 行で始まる。detached
/// worktree には `branch` 行の代わりに `detached` 行が出るため、明示的に
/// 解釈して branch を `"HEAD"` とする。レコードは branch の有無に関わらず
/// 次の `worktree` 行（または入力末尾）で必ず push され、並び順に依存しない。
fn parse_worktree_list(output: &str) -> Vec<WorktreeInfo> {
    let mut worktrees = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_branch: Option<String> = None;
    let mut current_head: Option<String> = None;

    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            // 前のレコードを flush。branch が無くても（detached でも）push する。
            push_worktree(
                &mut worktrees,
                &current_path,
                &current_branch,
                &current_head,
            );
            current_path = Some(path.to_string());
            current_branch = None;
            current_head = None;
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current_head = Some(head.to_string());
        } else if let Some(branch_full) = line.strip_prefix("branch ") {
            let branch = branch_full
                .strip_prefix("refs/heads/")
                .unwrap_or(branch_full);
            current_branch = Some(branch.to_string());
        } else if line == "detached" {
            // git は detached worktree に branch 行を出さないため、
            // 従来の末尾フォールバックと同じ "HEAD" を設定する。
            current_branch = Some("HEAD".to_string());
        }
    }

    // 末尾のレコードを flush（ルート worktree 等）。
    push_worktree(
        &mut worktrees,
        &current_path,
        &current_branch,
        &current_head,
    );

    worktrees
}

/// 同期実装。git サブプロセスの終了を待つため、axum の非同期ハンドラからは
/// 直接呼ばず `list_worktrees_async` を利用すること。同期テストからは
/// このモジュール内で参照できる。
fn list_worktrees(repo_path: &Path) -> Result<Vec<WorktreeInfo>, WorktreeError> {
    if !is_git_repo(repo_path) {
        return Err(WorktreeError::NotGitRepo);
    }

    let output = run_git(&["worktree", "list", "--porcelain"], repo_path)?;
    Ok(parse_worktree_list(&output))
}

/// 指定した名前のローカルブランチが厳密に存在するかを判定する。
///
/// `git branch --list <name>` は `<name>` を glob パターンとして解釈し、
/// 例えば `feature` に対して `feature/child` が存在する場合も接頭辞一致で
/// 検出してしまう。`git show-ref --verify` は ref 名の完全一致のみを検証するため、
/// この誤判定を避けられる（`--verify` はワイルドカードを一切許さない）。
fn branch_exists(repo_path: &Path, branch_name: &str) -> bool {
    let refname = format!("refs/heads/{}", branch_name);
    run_git(&["show-ref", "--verify", "--quiet", &refname], repo_path).is_ok()
}

/// 同期実装。axum の非同期ハンドラからは直接呼ばず
/// `create_worktree_async` を利用すること。
fn create_worktree(
    repo_path: &Path,
    branch: Option<&str>,
    target_path: Option<&str>,
) -> Result<WorktreeInfo, WorktreeError> {
    if !is_git_repo(repo_path) {
        return Err(WorktreeError::NotGitRepo);
    }

    let default_branch_name = format!("worktree-{}", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
    let branch_name = branch.unwrap_or(&default_branch_name);

    let repo_name = repo_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());
    let default_worktree_path = format!("{}-{}", repo_name, branch_name);

    let worktree_path = target_path
        .map(|s| s.to_string())
        .unwrap_or(default_worktree_path);

    let full_path = if Path::new(&worktree_path).is_absolute() {
        worktree_path.clone()
    } else {
        repo_path
            .parent()
            .map(|p| p.join(&worktree_path))
            .unwrap_or_else(|| repo_path.join(&worktree_path))
            .to_string_lossy()
            .to_string()
    };

    if branch_exists(repo_path, branch_name) {
        // 既存ブランチに -b を付けると同名ブランチの新規作成になり失敗するため、
        // -b を外して既存ブランチをそのまま checkout する。
        run_git(&["worktree", "add", &full_path, branch_name], repo_path)?;
    } else {
        let default_branch = run_git(&["symbolic-ref", "--short", "HEAD"], repo_path)
            .unwrap_or_else(|_| "main".to_string());

        run_git(
            &[
                "worktree",
                "add",
                "-b",
                branch_name,
                &full_path,
                &default_branch,
            ],
            repo_path,
        )?;
    }

    let head = run_git(&["rev-parse", "HEAD"], Path::new(&full_path))?;

    Ok(WorktreeInfo {
        path: full_path,
        branch: branch_name.to_string(),
        head,
    })
}

/// 同期実装。axum の非同期ハンドラからは直接呼ばず
/// `remove_worktree_async` を利用すること。
fn remove_worktree(
    repo_path: &Path,
    worktree_path: &str,
    force: bool,
) -> Result<(), WorktreeError> {
    if !is_git_repo(repo_path) {
        return Err(WorktreeError::NotGitRepo);
    }

    let mut args = vec!["worktree", "remove", worktree_path];
    if force {
        args.push("--force");
    }

    run_git(&args, repo_path)?;
    Ok(())
}

/// 同期実装。axum の非同期ハンドラからは直接呼ばず
/// `prune_worktrees_async` を利用すること。
fn prune_worktrees(repo_path: &Path) -> Result<(), WorktreeError> {
    if !is_git_repo(repo_path) {
        return Err(WorktreeError::NotGitRepo);
    }

    run_git(&["worktree", "prune"], repo_path)?;
    Ok(())
}

/// 同期 git 処理を tokio のブロッキングプールへ逃がす共通ヘルパー。
///
/// `git` サブプロセスの終了待ちの間、axum のワーカースレッドを占有しないようにする。
/// `JoinError` の文言には任意のパニックメッセージが含まれ得るため、呼び出し側へ
/// 返すエラーは固定文言にしてパスや秘密情報の漏洩を防ぐ。
async fn spawn_blocking_git<T, F>(f: F) -> Result<T, WorktreeError>
where
    F: FnOnce() -> Result<T, WorktreeError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| WorktreeError::GitError("blocking task failed".to_string()))?
}

/// `list_worktrees` をブロッキングプールで実行する非同期ラッパー。
pub async fn list_worktrees_async(repo_path: &Path) -> Result<Vec<WorktreeInfo>, WorktreeError> {
    let repo_path = repo_path.to_path_buf();
    spawn_blocking_git(move || list_worktrees(&repo_path)).await
}

/// `create_worktree` をブロッキングプールで実行する非同期ラッパー。
pub async fn create_worktree_async(
    repo_path: &Path,
    branch: Option<&str>,
    target_path: Option<&str>,
) -> Result<WorktreeInfo, WorktreeError> {
    let repo_path = repo_path.to_path_buf();
    let branch = branch.map(str::to_owned);
    let target_path = target_path.map(str::to_owned);
    spawn_blocking_git(move || {
        create_worktree(&repo_path, branch.as_deref(), target_path.as_deref())
    })
    .await
}

/// `remove_worktree` をブロッキングプールで実行する非同期ラッパー。
pub async fn remove_worktree_async(
    repo_path: &Path,
    worktree_path: &str,
    force: bool,
) -> Result<(), WorktreeError> {
    let repo_path = repo_path.to_path_buf();
    let worktree_path = worktree_path.to_owned();
    spawn_blocking_git(move || remove_worktree(&repo_path, &worktree_path, force)).await
}

/// `prune_worktrees` をブロッキングプールで実行する非同期ラッパー。
pub async fn prune_worktrees_async(repo_path: &Path) -> Result<(), WorktreeError> {
    let repo_path = repo_path.to_path_buf();
    spawn_blocking_git(move || prune_worktrees(&repo_path)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("opencode-worktree-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn is_git_repo_false_for_plain_dir() {
        let dir = temp_dir();
        assert!(!is_git_repo(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn is_git_repo_true_for_git_dir() {
        let dir = temp_dir();
        std::fs::create_dir(dir.join(".git")).unwrap();
        assert!(is_git_repo(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn is_git_repo_true_for_current_workspace() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        assert!(is_git_repo(root));
    }

    #[test]
    fn list_worktrees_rejects_non_repo() {
        let dir = temp_dir();
        assert!(matches!(
            list_worktrees(&dir),
            Err(WorktreeError::NotGitRepo)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn run_git_returns_trimmed_stdout() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let branch = run_git(&["rev-parse", "--abbrev-ref", "HEAD"], root).unwrap();
        assert!(!branch.is_empty());
        assert!(!branch.contains('\n'));
    }

    /// 1 コミット済みの一時 git リポジトリを作る。
    fn init_git_repo() -> std::path::PathBuf {
        let dir = temp_dir();
        run_git(&["init", "-q"], &dir).unwrap();
        run_git(&["config", "user.email", "worktree-test@example.com"], &dir).unwrap();
        run_git(&["config", "user.name", "Worktree Test"], &dir).unwrap();
        std::fs::write(dir.join("README.md"), "initial\n").unwrap();
        run_git(&["add", "README.md"], &dir).unwrap();
        run_git(&["commit", "-q", "-m", "initial"], &dir).unwrap();
        dir
    }

    /// 一時リポジトリと衝突しない、兄弟の絶対パスを worktree 先として返す。
    fn worktree_target(repo: &Path) -> String {
        format!("{}-wt", repo.to_string_lossy())
    }

    fn cleanup(repo: &Path, target: &str) {
        std::fs::remove_dir_all(target).unwrap();
        std::fs::remove_dir_all(repo).unwrap();
    }

    #[test]
    fn branch_exists_is_exact_not_prefix_or_glob() {
        let repo = init_git_repo();
        run_git(&["branch", "feature/parent"], &repo).unwrap();
        run_git(&["branch", "feature1"], &repo).unwrap();

        // `git branch --list feature` は feature/parent を接頭辞一致で返すが、
        // feature というブランチ自体は存在しない。
        assert!(!branch_exists(&repo, "feature"));
        // glob メタ文字も解釈されない（feature[0-9] というブランチは存在しない）。
        assert!(!branch_exists(&repo, "feature[0-9]"));
        assert!(branch_exists(&repo, "feature/parent"));
        assert!(branch_exists(&repo, "feature1"));

        std::fs::remove_dir_all(&repo).unwrap();
    }

    #[test]
    fn create_worktree_with_new_branch() {
        let repo = init_git_repo();
        let target = worktree_target(&repo);

        let info = create_worktree(&repo, Some("feature-new"), Some(target.as_str())).unwrap();

        assert_eq!(info.path, target);
        assert_eq!(info.branch, "feature-new");
        assert!(Path::new(info.path.as_str()).join("README.md").exists());
        assert!(branch_exists(&repo, "feature-new"));
        let checked_out = run_git(
            &["symbolic-ref", "--short", "HEAD"],
            Path::new(info.path.as_str()),
        )
        .unwrap();
        assert_eq!(checked_out, "feature-new");

        cleanup(&repo, target.as_str());
    }

    #[test]
    fn create_worktree_with_existing_branch() {
        let repo = init_git_repo();
        run_git(&["branch", "feature-existing"], &repo).unwrap();
        assert!(branch_exists(&repo, "feature-existing"));

        let target = worktree_target(&repo);

        // 修正前はここで `git worktree add -b feature-existing ...` が実行され
        // 「ブランチは既に存在する」と失敗していた。
        let info = create_worktree(&repo, Some("feature-existing"), Some(target.as_str())).unwrap();

        assert_eq!(info.path, target);
        assert_eq!(info.branch, "feature-existing");
        assert!(Path::new(info.path.as_str()).join("README.md").exists());
        let checked_out = run_git(
            &["symbolic-ref", "--short", "HEAD"],
            Path::new(info.path.as_str()),
        )
        .unwrap();
        assert_eq!(checked_out, "feature-existing");

        cleanup(&repo, target.as_str());
    }

    /// 回帰テスト: attached -> detached -> attached の並びで、
    /// 中間にある detached worktree が黙って除外されないこと。
    /// 修正前は中間の detached レコードが落とされ len == 2 となっていた。
    #[test]
    fn parse_worktree_list_keeps_detached_in_the_middle() {
        let porcelain = "\
worktree /repo/main
HEAD 1111111111111111111111111111111111111111
branch refs/heads/main

worktree /repo/detached
HEAD 2222222222222222222222222222222222222222
detached

worktree /repo/feature
HEAD 3333333333333333333333333333333333333333
branch refs/heads/feature
";

        let trees = parse_worktree_list(porcelain);

        assert_eq!(trees.len(), 3);
        assert_eq!(trees[0].path, "/repo/main");
        assert_eq!(trees[0].branch, "main");
        assert_eq!(trees[0].head, "1111111111111111111111111111111111111111");
        assert_eq!(trees[1].path, "/repo/detached");
        assert_eq!(trees[1].branch, "HEAD");
        assert_eq!(trees[1].head, "2222222222222222222222222222222222222222");
        assert_eq!(trees[2].path, "/repo/feature");
        assert_eq!(trees[2].branch, "feature");
        assert_eq!(trees[2].head, "3333333333333333333333333333333333333333");
    }

    /// 並び順非依存の確認: detached が先頭と末尾にあっても全レコードが得られること。
    #[test]
    fn parse_worktree_list_keeps_detached_at_boundaries() {
        let porcelain = "\
worktree /repo/detached-first
HEAD aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
detached

worktree /repo/main
HEAD bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
branch refs/heads/main

worktree /repo/detached-last
HEAD cccccccccccccccccccccccccccccccccccccccc
detached
";

        let trees = parse_worktree_list(porcelain);

        assert_eq!(trees.len(), 3);
        assert_eq!(trees[0].path, "/repo/detached-first");
        assert_eq!(trees[0].branch, "HEAD");
        assert_eq!(trees[1].path, "/repo/main");
        assert_eq!(trees[1].branch, "main");
        assert_eq!(trees[2].path, "/repo/detached-last");
        assert_eq!(trees[2].branch, "HEAD");
        assert_eq!(trees[2].head, "cccccccccccccccccccccccccccccccccccccccc");
    }

    /// 回帰テスト: 同期 git 実行が async ワーカー上で直接走らないこと。
    ///
    /// current_thread ランタイムの唯一のスレッドをブロック処理で占有してしまうと、
    /// 並行する async タスクのハートビートがタイムアウトする。ブロッキングプールへ
    /// 逃がしていれば、git の終了待ち中もハートビートは進行する。
    #[tokio::test(flavor = "current_thread")]
    async fn spawn_blocking_git_does_not_block_async_worker() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{mpsc, Arc};
        use std::time::Duration;

        let (release_tx, release_rx) = mpsc::channel::<()>();
        let started = Arc::new(AtomicBool::new(false));
        let started_flag = Arc::clone(&started);

        // 未来を明示的に spawn してポーリングさせ、並行タスクとして走らせる。
        let blocking = tokio::spawn(spawn_blocking_git(move || {
            started_flag.store(true, Ordering::SeqCst);
            // git サブプロセスの終了待ちを模擬した同期待機。
            let _ = release_rx.recv();
            Ok::<(), WorktreeError>(())
        }));

        tokio::time::timeout(Duration::from_secs(2), async {
            while !started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("blocking task should start");

        let heartbeat = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(10)).await;
            42
        })
        .await
        .expect("async worker must not be blocked while git runs");
        assert_eq!(heartbeat, 42);

        release_tx
            .send(())
            .expect("release channel should be alive");
        blocking
            .await
            .expect("blocking task should complete")
            .expect("blocking task result should be ok");
    }

    /// 4 つの非同期ラッパーが同期実装と同じエラーを伝播すること。
    #[tokio::test(flavor = "current_thread")]
    async fn async_wrappers_propagate_not_git_repo() {
        let dir = temp_dir();
        assert!(matches!(
            list_worktrees_async(&dir).await,
            Err(WorktreeError::NotGitRepo)
        ));
        assert!(matches!(
            create_worktree_async(&dir, None, None).await,
            Err(WorktreeError::NotGitRepo)
        ));
        assert!(matches!(
            remove_worktree_async(&dir, "/nonexistent-worktree", false).await,
            Err(WorktreeError::NotGitRepo)
        ));
        assert!(matches!(
            prune_worktrees_async(&dir).await,
            Err(WorktreeError::NotGitRepo)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 非同期ラッパー経由でも既存の作成・一覧・削除が成立すること。
    #[tokio::test]
    async fn async_worktree_round_trip() {
        let repo = init_git_repo();
        let target = worktree_target(&repo);

        let info = create_worktree_async(&repo, Some("feature-async"), Some(target.as_str()))
            .await
            .unwrap();
        assert_eq!(info.path, target);
        assert_eq!(info.branch, "feature-async");

        let listed = list_worktrees_async(&repo).await.unwrap();
        assert!(listed
            .iter()
            .any(|w| w.path == target && w.branch == "feature-async"));

        remove_worktree_async(&repo, target.as_str(), true)
            .await
            .unwrap();
        // git が worktree ディレクトリを削除するため、リポジトリのみ片付ける。
        std::fs::remove_dir_all(&repo).unwrap();
    }
}

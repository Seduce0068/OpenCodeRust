use crate::git::Repository;
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Git リポジトリ内で発生した変更種別。
///
/// `.git` 配下の任意の変更（config / HEAD / refs / logs / index 等）を
/// `IndexChanged` として配信する。以前定義されていた
/// デッドバリアント（workspace 内に購読者・発行箇所とも無し）は
/// 削除した（L-18）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GitEvent {
    /// `.git` 配下で何らかの変更が発生した（インデックスに限らない）。
    IndexChanged,
}

/// `.git` ディレクトリを監視して変更をブロードキャストするウォッチャー。
pub struct GitWatcher {
    _repo: Repository,
    event_tx: broadcast::Sender<GitEvent>,
    _watcher: RecommendedWatcher,
}

impl GitWatcher {
    /// 対象リポジトリの `.git` ディレクトリ監視を開始する。
    pub fn new(repo: Repository) -> anyhow::Result<Self> {
        let (event_tx, _) = broadcast::channel(100);
        let event_tx_clone = event_tx.clone();

        // Watch .git directory
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(_event) = res {
                // .git 配下の変更は現在すべて IndexChanged として配信する
                // （他バリアントは未購読・未発行のため削除済み、L-18）。
                let _ = event_tx_clone.send(GitEvent::IndexChanged);
            }
        })?;

        watcher.watch(&repo.git_directory, RecursiveMode::Recursive)?;

        Ok(Self {
            _repo: repo,
            event_tx,
            _watcher: watcher,
        })
    }

    /// Git 変更イベントを受信するためのレシーバーを作成する。
    pub fn subscribe(&self) -> broadcast::Receiver<GitEvent> {
        self.event_tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::tempdir;
    use tokio::time::{timeout, Duration};

    #[tokio::test]
    async fn test_git_watcher() {
        let root = tempdir().unwrap();
        let worktree = root.path().to_path_buf();
        let git_dir = worktree.join(".git");

        StdCommand::new("git")
            .arg("init")
            .current_dir(&worktree)
            .status()
            .unwrap();

        let repo = Repository {
            worktree: worktree.clone(),
            git_directory: git_dir.clone(),
            common_directory: git_dir.clone(),
        };
        let watcher = GitWatcher::new(repo).unwrap();
        let mut rx = watcher.subscribe();

        // Trigger a git change
        StdCommand::new("git")
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .current_dir(&worktree)
            .status()
            .unwrap();

        // Wait for event
        let event = timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event, GitEvent::IndexChanged);
    }
}

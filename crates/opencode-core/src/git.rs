use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::process::Command;

/// Git リポジトリのディレクトリ構成を表す情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Repository {
    /// 作業ツリーのディレクトリ。
    pub worktree: PathBuf,
    /// `.git` ディレクトリのパス。
    pub git_directory: PathBuf,
    /// 共有される Git ディレクトリのパス。
    pub common_directory: PathBuf,
}

/// 特定のリポジトリに対して Git コマンドを実行するクライアント。
pub struct GitClient {
    /// 操作対象のリポジトリ。
    pub repository: Repository,
}

impl GitClient {
    /// 対象リポジトリを指定してクライアントを生成する。
    pub fn new(repository: Repository) -> Self {
        Self { repository }
    }

    async fn run(&self, args: &[&str]) -> anyhow::Result<(i32, String, String)> {
        let output = Command::new("git")
            .args([
                "--git-dir",
                self.repository.git_directory.to_str().unwrap(),
                "--work-tree",
                self.repository.worktree.to_str().unwrap(),
            ])
            .args(args)
            .current_dir(&self.repository.worktree)
            .output()
            .await?;

        Ok((
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }

    /// `HEAD` が指すコミットのハッシュを返す。未コミットの場合は `None`。
    pub async fn head(&self) -> anyhow::Result<Option<String>> {
        let (exit_code, stdout, _) = self.run(&["rev-parse", "HEAD"]).await?;
        if exit_code == 0 {
            Ok(Some(stdout.trim().to_string()))
        } else {
            Ok(None)
        }
    }

    /// 現在 checkout されているブランチの短い名前を返す。detached HEAD の場合は `None`。
    pub async fn branch(&self) -> anyhow::Result<Option<String>> {
        let (exit_code, stdout, _) = self
            .run(&["symbolic-ref", "--quiet", "--short", "HEAD"])
            .await?;
        if exit_code == 0 {
            Ok(Some(stdout.trim().to_string()))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_git_operations() {
        let root = tempdir().unwrap();
        let worktree = root.path().to_path_buf();
        let git_dir = worktree.join(".git");

        // Initialize git repo
        StdCommand::new("git")
            .arg("init")
            .current_dir(&worktree)
            .status()
            .unwrap();
        StdCommand::new("git")
            .arg("config")
            .arg("user.email")
            .arg("test@example.com")
            .current_dir(&worktree)
            .status()
            .unwrap();
        StdCommand::new("git")
            .arg("config")
            .arg("user.name")
            .arg("Test User")
            .current_dir(&worktree)
            .status()
            .unwrap();
        StdCommand::new("git")
            .arg("commit")
            .arg("--allow-empty")
            .arg("-m")
            .arg("initial")
            .current_dir(&worktree)
            .status()
            .unwrap();

        let repo = Repository {
            worktree: worktree.clone(),
            git_directory: git_dir.clone(),
            common_directory: git_dir.clone(),
        };
        let client = GitClient::new(repo);

        assert!(client.head().await.unwrap().is_some());
        assert_eq!(client.branch().await.unwrap().unwrap(), "master");
    }
}

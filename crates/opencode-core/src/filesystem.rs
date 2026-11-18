use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use tokio::fs;

/// ディレクトリ走査で見つかった項目の種別。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EntryType {
    /// 通常のファイル。
    File,
    /// ディレクトリ。
    Directory,
}

/// ルートディレクトリからの相対パスと項目種別を表すエントリ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// ルートからの相対パス。
    pub path: PathBuf,
    /// 項目の種別。
    pub entry_type: EntryType,
}

/// ファイルの読み取りとディレクトリ一覧取得を提供する非同期インターフェース。
#[async_trait::async_trait]
pub trait FileSystem {
    /// 指定したパスのファイル内容をバイト列として読み込む。
    async fn read(&self, path: &Path) -> anyhow::Result<Vec<u8>>;
    /// 指定したディレクトリ直下の項目を列挙する。
    async fn list(&self, path: &Path) -> anyhow::Result<Vec<Entry>>;
}

/// 指定したルートディレクトリを基点にファイルシステム操作を行う実装。
pub struct FileSystemImpl {
    root: PathBuf,
}

impl FileSystemImpl {
    /// 基点となるルートディレクトリを指定してインスタンスを生成する。
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// `path` がルート外を指していないことを検証し、`self.root.join(path)` を返す。
    ///
    /// 1. 字句検査で絶対パス（[`Component::RootDir`] / [`Component::Prefix`]）
    ///    と `..`（[`Component::ParentDir`]）を拒否する。
    /// 2. canonicalize でシンボリックリンク経由の脱出も fail-closed で検出する。
    ///
    /// 対象が存在しない場合は canonicalize できないが、これは後続の `read` / `list`
    /// が `NotFound` を返すよう呼び出し元に委ね、既存の祖先を検査して包含を確認する。
    fn validated_join(&self, path: &Path) -> anyhow::Result<PathBuf> {
        for component in path.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                Component::RootDir | Component::ParentDir | Component::Prefix(_) => {
                    anyhow::bail!("path is not allowed: {}", path.display());
                }
            }
        }

        let canonical_root = self.root.canonicalize()?;
        let full_path = self.root.join(path);

        // canonicalize できる範囲で包含関係を確認する。存在しない経路は上位の
        // 既存祖先へ遡り、NotFound 自体は後続の I/O に委ねる。
        let mut probe: &Path = &full_path;
        loop {
            match probe.canonicalize() {
                Ok(canonical) => {
                    if !canonical.starts_with(&canonical_root) {
                        anyhow::bail!("path escapes root: {}", path.display());
                    }
                    break;
                }
                Err(err) if err.kind() == ErrorKind::NotFound => match probe.parent() {
                    Some(parent) if parent != probe => probe = parent,
                    _ => anyhow::bail!("path escapes root: {}", path.display()),
                },
                Err(err) => return Err(err.into()),
            }
        }

        Ok(full_path)
    }
}

#[async_trait::async_trait]
impl FileSystem for FileSystemImpl {
    async fn read(&self, path: &Path) -> anyhow::Result<Vec<u8>> {
        let full_path = self.validated_join(path)?;
        let content = fs::read(full_path).await?;
        Ok(content)
    }

    async fn list(&self, path: &Path) -> anyhow::Result<Vec<Entry>> {
        let full_path = self.validated_join(path)?;
        let mut entries = Vec::new();
        let mut dir = fs::read_dir(full_path).await?;

        while let Some(entry) = dir.next_entry().await? {
            let file_type = entry.file_type().await?;
            let entry_type = if file_type.is_dir() {
                EntryType::Directory
            } else {
                EntryType::File
            };
            let entry_path = entry.path();
            // strip_prefix が成功した時点で entry.path は root 配下の相対パス。
            let relative = entry_path.strip_prefix(&self.root).map_err(|_| {
                anyhow::anyhow!("entry path escapes root: {}", entry_path.display())
            })?;
            entries.push(Entry {
                path: relative.to_path_buf(),
                entry_type,
            });
        }
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_fs_read_list() {
        let root = tempdir().unwrap();
        let fs_impl = FileSystemImpl::new(root.path().to_path_buf());

        // Create file
        fs::write(root.path().join("test.txt"), b"hello")
            .await
            .unwrap();

        // Read file
        let content = fs_impl.read(Path::new("test.txt")).await.unwrap();
        assert_eq!(content, b"hello");

        // List directory
        let entries = fs_impl.list(Path::new("")).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("test.txt"));
        assert_eq!(entries[0].entry_type, EntryType::File);
    }

    #[tokio::test]
    async fn read_rejects_absolute_path() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_file = outside.path().join("secret.txt");
        fs::write(&outside_file, b"secret").await.unwrap();

        let fs_impl = FileSystemImpl::new(root.path().to_path_buf());
        let err = fs_impl.read(&outside_file).await.unwrap_err();
        assert!(err.to_string().contains("not allowed"));
    }

    #[tokio::test]
    async fn list_rejects_absolute_path() {
        let root = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let outside_dir = outside.path().join("dir");
        fs::create_dir(&outside_dir).await.unwrap();

        let fs_impl = FileSystemImpl::new(root.path().to_path_buf());
        let err = fs_impl.list(&outside_dir).await.unwrap_err();
        assert!(err.to_string().contains("not allowed"));
    }

    #[tokio::test]
    async fn read_rejects_parent_traversal() {
        let parent = tempdir().unwrap();
        let root = parent.path().join("root");
        fs::create_dir(&root).await.unwrap();
        fs::write(parent.path().join("secret.txt"), b"secret")
            .await
            .unwrap();

        let fs_impl = FileSystemImpl::new(root);
        for path in [
            Path::new("../secret.txt"),
            Path::new("sub/../../secret.txt"),
        ] {
            let err = fs_impl.read(path).await.unwrap_err();
            assert!(err.to_string().contains("not allowed"));
        }
    }

    #[tokio::test]
    async fn list_rejects_parent_traversal() {
        let parent = tempdir().unwrap();
        let root = parent.path().join("root");
        fs::create_dir(&root).await.unwrap();

        let fs_impl = FileSystemImpl::new(root);
        let err = fs_impl.list(Path::new("..")).await.unwrap_err();
        assert!(err.to_string().contains("not allowed"));
    }

    #[tokio::test]
    async fn read_and_list_allow_legitimate_subdirectory() {
        let root = tempdir().unwrap();
        let sub = root.path().join("sub");
        fs::create_dir(&sub).await.unwrap();
        fs::write(sub.join("nested.txt"), b"nested").await.unwrap();

        let fs_impl = FileSystemImpl::new(root.path().to_path_buf());
        assert_eq!(
            fs_impl.read(Path::new("sub/nested.txt")).await.unwrap(),
            b"nested"
        );

        let entries = fs_impl.list(Path::new("sub")).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("sub/nested.txt"));
        assert_eq!(entries[0].entry_type, EntryType::File);
        assert!(!entries[0].path.is_absolute());
        assert!(!entries[0]
            .path
            .components()
            .any(|component| matches!(component, Component::ParentDir)));
    }

    #[tokio::test]
    async fn read_missing_file_preserves_not_found() {
        let root = tempdir().unwrap();
        let fs_impl = FileSystemImpl::new(root.path().to_path_buf());

        let err = fs_impl.read(Path::new("missing.txt")).await.unwrap_err();
        assert_eq!(
            err.downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(ErrorKind::NotFound)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn read_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let parent = tempdir().unwrap();
        let root = parent.path().join("root");
        fs::create_dir(&root).await.unwrap();
        let outside = parent.path().join("secret.txt");
        fs::write(&outside, b"secret").await.unwrap();
        symlink(&outside, root.join("link")).unwrap();

        let fs_impl = FileSystemImpl::new(root);
        let err = fs_impl.read(Path::new("link")).await.unwrap_err();
        assert!(err.to_string().contains("escapes root"));
    }
}

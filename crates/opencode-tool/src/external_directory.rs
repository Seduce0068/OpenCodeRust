use crate::{PermissionRequest, ToolContext, ToolError};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct ExternalDirectoryOptions {
    pub bypass: bool,
    pub kind: ExternalDirectoryKind,
}

impl Default for ExternalDirectoryOptions {
    fn default() -> Self {
        Self {
            bypass: false,
            kind: ExternalDirectoryKind::File,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ExternalDirectoryKind {
    File,
    Directory,
}

pub async fn assert_external_directory(
    ctx: &ToolContext,
    target: Option<&str>,
    options: ExternalDirectoryOptions,
) -> Result<(), ToolError> {
    let target = match target {
        Some(t) => t,
        None => return Ok(()),
    };

    if options.bypass {
        return Ok(());
    }

    if is_within_project(target, &ctx.project_root) {
        return Ok(());
    }

    let parent_dir = match options.kind {
        ExternalDirectoryKind::Directory => target.to_string(),
        ExternalDirectoryKind::File => Path::new(target)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| target.to_string()),
    };

    let glob_pattern = format!("{}/*", parent_dir);

    let request = PermissionRequest::new("external_directory")
        .with_pattern(&glob_pattern)
        .with_metadata("filepath", serde_json::json!(target))
        .with_metadata("parentDir", serde_json::json!(&parent_dir));

    ctx.ask_permission(request).await
}

fn is_within_project(target: &str, project_root: &str) -> bool {
    let target_path = Path::new(target);

    if target_path.is_absolute() {
        return target_path.starts_with(project_root);
    }

    if target.starts_with("./") || target.starts_with("../") {
        return true;
    }

    if !target.starts_with('/') && !target.contains(':') {
        return true;
    }

    false
}

pub fn get_parent_directory(target: &str, kind: ExternalDirectoryKind) -> String {
    match kind {
        ExternalDirectoryKind::Directory => target.to_string(),
        ExternalDirectoryKind::File => Path::new(target)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| target.to_string()),
    }
}

pub fn make_glob_pattern(parent_dir: &str) -> String {
    format!("{}/*", parent_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_within_project_relative() {
        assert!(is_within_project("./src/main.rs", "/home/user/project"));
        assert!(is_within_project("../other/file.txt", "/home/user/project"));
        assert!(is_within_project("src/lib.rs", "/home/user/project"));
    }

    #[test]
    fn test_is_within_project_absolute() {
        assert!(is_within_project(
            "/home/user/project/src/main.rs",
            "/home/user/project"
        ));
        assert!(!is_within_project(
            "/home/other/file.txt",
            "/home/user/project"
        ));
    }

    #[test]
    fn test_get_parent_directory_file() {
        let parent =
            get_parent_directory("/home/user/project/file.txt", ExternalDirectoryKind::File);
        assert_eq!(parent, "/home/user/project");
    }

    #[test]
    fn test_get_parent_directory_dir() {
        let parent = get_parent_directory("/home/user/project", ExternalDirectoryKind::Directory);
        assert_eq!(parent, "/home/user/project");
    }

    #[test]
    fn test_make_glob_pattern() {
        let pattern = make_glob_pattern("/home/user/external");
        assert_eq!(pattern, "/home/user/external/*");
    }

    fn context(asks: &std::sync::Arc<std::sync::Mutex<Vec<PermissionRequest>>>) -> ToolContext {
        let asks = asks.clone();
        ToolContext::new("s".to_owned(), "m".to_owned(), "/work".to_owned()).with_ask(
            move |request| {
                let asks = asks.clone();
                async move {
                    asks.lock().unwrap().push(request);
                    Ok(())
                }
            },
        )
    }

    #[tokio::test]
    async fn no_target_returns_ok() {
        let asks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        assert_external_directory(&context(&asks), None, ExternalDirectoryOptions::default())
            .await
            .unwrap();
        assert!(asks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn bypass_skips_permission() {
        let asks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let options = ExternalDirectoryOptions {
            bypass: true,
            ..Default::default()
        };
        assert_external_directory(&context(&asks), Some("/etc/passwd"), options)
            .await
            .unwrap();
        assert!(asks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn within_project_skips_permission() {
        let asks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        assert_external_directory(
            &context(&asks),
            Some("/work/src/main.rs"),
            ExternalDirectoryOptions::default(),
        )
        .await
        .unwrap();
        assert!(asks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn external_file_requests_permission() {
        let asks = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        assert_external_directory(
            &context(&asks),
            Some("/etc/passwd"),
            ExternalDirectoryOptions::default(),
        )
        .await
        .unwrap();

        let asks = asks.lock().unwrap();
        assert_eq!(asks.len(), 1);
        assert_eq!(asks[0].permission, "external_directory");
        assert_eq!(asks[0].patterns[0], "/etc/*");
        assert_eq!(asks[0].metadata["filepath"], "/etc/passwd");
    }
}

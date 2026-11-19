use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::{DashMap, DashSet};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

/// File change event types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileEvent {
    Add,
    Change,
    Unlink,
}

/// A file watcher event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherEvent {
    pub file: PathBuf,
    pub event: FileEvent,
}

/// File watcher errors
#[derive(Debug, Error)]
pub enum WatcherError {
    #[error("Failed to create watcher: {0}")]
    CreateError(#[from] notify::Error),

    #[error("Failed to watch path: {0}")]
    WatchError(String),

    #[error("Path does not exist: {0}")]
    PathNotFound(PathBuf),

    #[error("Already watching: {0}")]
    AlreadyWatching(PathBuf),
}

/// Configuration for the file watcher
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Paths to ignore (glob patterns)
    pub ignore_patterns: Vec<String>,
    /// Debounce window in milliseconds: repeated events for the same path
    /// within this window are coalesced into a single notification.
    pub debounce_ms: u64,
    /// Whether to watch recursively
    pub recursive: bool,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            ignore_patterns: vec![
                "**/node_modules/**".to_string(),
                "**/.git/**".to_string(),
                "**/target/**".to_string(),
                "**/.DS_Store".to_string(),
                "**/*.swp".to_string(),
                "**/*.swo".to_string(),
                "**/*~".to_string(),
            ],
            debounce_ms: 100,
            recursive: true,
        }
    }
}

/// File system watcher
pub struct FileWatcher {
    /// One lazily-created backend watcher shared by every watched path.
    /// Replacing this on each watch() call used to drop the previous watcher
    /// and silently stop monitoring earlier paths (M-63).
    watcher: RwLock<Option<RecommendedWatcher>>,
    watched_paths: DashSet<PathBuf>,
    ignore_patterns: Vec<glob::Pattern>,
    event_tx: broadcast::Sender<WatcherEvent>,
    debounce_ms: u64,
    /// Mode applied to every path, not just the first one (M-63).
    recursive: bool,
    /// Last emitted event time per path (used for debouncing).
    event_last_seen: DashMap<PathBuf, Instant>,
}

impl FileWatcher {
    /// Create a new file watcher
    pub fn new(config: WatcherConfig) -> Result<Self, WatcherError> {
        let (event_tx, _) = broadcast::channel(1024);

        let ignore_patterns = config
            .ignore_patterns
            .iter()
            .filter_map(|p| glob::Pattern::new(p).ok())
            .collect();
        let recursive = config.recursive;

        Ok(Self {
            watcher: RwLock::new(None),
            watched_paths: DashSet::new(),
            ignore_patterns,
            event_tx,
            debounce_ms: config.debounce_ms,
            recursive,
            event_last_seen: DashMap::new(),
        })
    }

    /// Subscribe to file watcher events
    pub fn subscribe(&self) -> broadcast::Receiver<WatcherEvent> {
        self.event_tx.subscribe()
    }

    /// Start watching a directory.
    ///
    /// Every path is registered on a single lazily-created backend watcher, so
    /// adding a new path never drops the watcher that monitors earlier paths.
    pub fn watch(&self, path: &Path) -> Result<(), WatcherError> {
        if !path.exists() {
            return Err(WatcherError::PathNotFound(path.to_path_buf()));
        }

        // Apply the configured mode to every path (including the second and
        // later ones) instead of silently degrading to NonRecursive.
        let mode = if self.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };

        {
            let mut watcher_guard = self.watcher.write();

            // Keep the duplicate check and the registry update inside the
            // watcher lock so concurrent watch() calls cannot double-register
            // the same path on the shared backend.
            if self.watched_paths.contains(path) {
                return Err(WatcherError::AlreadyWatching(path.to_path_buf()));
            }

            // Create the shared watcher once. Replacing it on every watch()
            // call used to stop all monitoring of already-registered paths.
            if watcher_guard.is_none() {
                let event_tx = self.event_tx.clone();
                let ignore_patterns = self.ignore_patterns.clone();
                let event_last_seen = self.event_last_seen.clone();
                let debounce_window = Duration::from_millis(self.debounce_ms);

                let mut watcher = notify::recommended_watcher(
                    move |res: Result<Event, notify::Error>| match res {
                        Ok(event) => {
                            let file_event = match event.kind {
                                EventKind::Create(_) => FileEvent::Add,
                                EventKind::Modify(_) => FileEvent::Change,
                                EventKind::Remove(_) => FileEvent::Unlink,
                                _ => return,
                            };

                            for path in &event.paths {
                                let path_str = path.to_string_lossy();
                                let should_ignore =
                                    ignore_patterns.iter().any(|p| p.matches(&path_str));

                                if should_ignore {
                                    debug!(path = ?path, "Ignoring file event");
                                    continue;
                                }

                                // Coalesce repeated events for the same path within the
                                // debounce window instead of forwarding every arrival.
                                let now = Instant::now();
                                if let Some(last) = event_last_seen.get(path) {
                                    if now.duration_since(*last) < debounce_window {
                                        debug!(path = ?path, "Debouncing file event");
                                        continue;
                                    }
                                }
                                event_last_seen.insert(path.clone(), now);

                                let watcher_event = WatcherEvent {
                                    file: path.clone(),
                                    event: file_event,
                                };

                                if let Err(e) = event_tx.send(watcher_event) {
                                    warn!(error = %e, "Failed to send watcher event");
                                }
                            }
                        }
                        Err(e) => {
                            error!(error = %e, "Watcher error");
                        }
                    },
                )?;

                watcher.configure(
                    Config::default().with_poll_interval(Duration::from_millis(self.debounce_ms)),
                )?;

                *watcher_guard = Some(watcher);
            }

            let watcher = watcher_guard
                .as_mut()
                .ok_or_else(|| WatcherError::WatchError("watcher not initialized".into()))?;
            watcher
                .watch(path, mode)
                .map_err(|e| WatcherError::WatchError(e.to_string()))?;

            self.watched_paths.insert(path.to_path_buf());
        }

        info!(path = ?path, "Started watching directory");

        Ok(())
    }

    /// Stop watching a single path and detach it from the backend watcher.
    pub fn unwatch(&self, path: &Path) -> Result<(), WatcherError> {
        // Serialize the whole detach operation with watch() via the same lock.
        let mut watcher_guard = self.watcher.write();

        if !self.watched_paths.contains(path) {
            return Ok(());
        }

        if let Some(watcher) = watcher_guard.as_mut() {
            watcher
                .unwatch(path)
                .map_err(|e| WatcherError::WatchError(e.to_string()))?;
        }

        self.watched_paths.remove(path);

        if self.watched_paths.is_empty() {
            *watcher_guard = None;
            info!("Stopped all watchers");
        } else {
            info!(path = ?path, "Stopped watching directory");
        }

        Ok(())
    }

    pub fn stop(&self) {
        self.watched_paths.clear();
        *self.watcher.write() = None;
        info!("Stopped all file watchers");
    }

    pub fn watched_paths(&self) -> Vec<PathBuf> {
        self.watched_paths.iter().map(|p| p.clone()).collect()
    }

    pub fn is_watching(&self, path: &Path) -> bool {
        self.watched_paths.contains(path)
    }
}

impl Drop for FileWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

static FILE_WATCHER: std::sync::OnceLock<Arc<FileWatcher>> = std::sync::OnceLock::new();

pub fn get_watcher() -> Arc<FileWatcher> {
    FILE_WATCHER
        .get_or_init(|| {
            Arc::new(
                FileWatcher::new(WatcherConfig::default()).expect("Failed to create file watcher"),
            )
        })
        .clone()
}

pub fn init_watcher(config: WatcherConfig) -> Arc<FileWatcher> {
    let watcher = Arc::new(FileWatcher::new(config).expect("Failed to create file watcher"));
    FILE_WATCHER.get_or_init(|| watcher.clone()).clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::time::sleep;

    #[test]
    fn test_watcher_config_default() {
        let config = WatcherConfig::default();
        assert!(!config.ignore_patterns.is_empty());
        assert!(config.recursive);
    }

    #[test]
    fn test_file_event_serde() {
        let event = FileEvent::Add;
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(json, "\"add\"");

        let parsed: FileEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, FileEvent::Add);
    }

    #[tokio::test]
    async fn test_watch_directory() {
        let temp_dir = TempDir::new().unwrap();
        let watcher = FileWatcher::new(WatcherConfig::default()).unwrap();

        let mut rx = watcher.subscribe();

        watcher.watch(temp_dir.path()).unwrap();

        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "hello").unwrap();

        sleep(Duration::from_millis(200)).await;

        match rx.try_recv() {
            Ok(event) => {
                assert!(event.file.ends_with("test.txt"));
                assert_eq!(event.event, FileEvent::Add);
            }
            Err(broadcast::error::TryRecvError::Empty) => {}
            Err(e) => panic!("Unexpected error: {}", e),
        }
    }

    #[tokio::test]
    async fn test_debounce_coalesces_repeated_events_for_same_path() {
        let temp_dir = TempDir::new().unwrap();
        let config = WatcherConfig {
            debounce_ms: 5_000,
            ..WatcherConfig::default()
        };
        let watcher = FileWatcher::new(config).unwrap();
        let mut rx = watcher.subscribe();

        watcher.watch(temp_dir.path()).unwrap();

        let test_file = temp_dir.path().join("debounce.txt");
        fs::write(&test_file, "first").unwrap();
        sleep(Duration::from_millis(50)).await;
        fs::write(&test_file, "second").unwrap();

        let mut count = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(event)) if event.file == test_file => count += 1,
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }

        assert_eq!(count, 1, "expected a single coalesced event for the path");
    }

    #[tokio::test]
    async fn test_debounce_is_independent_per_path() {
        let temp_dir = TempDir::new().unwrap();
        let config = WatcherConfig {
            debounce_ms: 5_000,
            ..WatcherConfig::default()
        };
        let watcher = FileWatcher::new(config).unwrap();
        let mut rx = watcher.subscribe();

        watcher.watch(temp_dir.path()).unwrap();

        let first = temp_dir.path().join("first.txt");
        let second = temp_dir.path().join("second.txt");

        // First writes on distinct paths must each be forwarded, while a
        // repeated write on one path is coalesced by its own window.
        fs::write(&first, "a").unwrap();
        sleep(Duration::from_millis(50)).await;
        fs::write(&second, "b").unwrap();
        sleep(Duration::from_millis(50)).await;
        fs::write(&first, "aa").unwrap();

        let mut first_count = 0;
        let mut second_count = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(event)) if event.file == first => first_count += 1,
                Ok(Ok(event)) if event.file == second => second_count += 1,
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }

        assert_eq!(
            first_count, 1,
            "repeat write on first path must be coalesced"
        );
        assert_eq!(
            second_count, 1,
            "first write on second path must be forwarded"
        );
    }

    #[tokio::test]
    async fn test_zero_debounce_forwards_repeated_events() {
        let temp_dir = TempDir::new().unwrap();
        let config = WatcherConfig {
            debounce_ms: 0,
            ..WatcherConfig::default()
        };
        let watcher = FileWatcher::new(config).unwrap();
        let mut rx = watcher.subscribe();

        watcher.watch(temp_dir.path()).unwrap();

        let test_file = temp_dir.path().join("no_debounce.txt");
        fs::write(&test_file, "first").unwrap();
        sleep(Duration::from_millis(80)).await;
        fs::write(&test_file, "second").unwrap();

        let mut count = 0;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(event)) if event.file == test_file => count += 1,
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }

        // A zero window has no lower bound to wait for, so assert only that
        // repeated writes are not coalesced (inotify may emit extra events).
        assert!(
            count >= 2,
            "debounce_ms=0 must forward repeated events, got {count}"
        );
    }

    #[test]
    fn test_watch_nonexistent_path() {
        let watcher = FileWatcher::new(WatcherConfig::default()).unwrap();
        let result = watcher.watch(Path::new("/nonexistent/path"));
        assert!(matches!(result, Err(WatcherError::PathNotFound(_))));
    }

    #[test]
    fn test_watch_same_path_twice() {
        let temp_dir = TempDir::new().unwrap();
        let watcher = FileWatcher::new(WatcherConfig::default()).unwrap();

        watcher.watch(temp_dir.path()).unwrap();
        let result = watcher.watch(temp_dir.path());
        assert!(matches!(result, Err(WatcherError::AlreadyWatching(_))));
    }

    #[test]
    fn test_watched_paths() {
        let temp_dir = TempDir::new().unwrap();
        let watcher = FileWatcher::new(WatcherConfig::default()).unwrap();

        assert!(watcher.watched_paths().is_empty());
        assert!(!watcher.is_watching(temp_dir.path()));

        watcher.watch(temp_dir.path()).unwrap();

        assert_eq!(watcher.watched_paths().len(), 1);
        assert!(watcher.is_watching(temp_dir.path()));
    }

    // Test basis: M-63. Oracle: writing a file under each of two watched roots
    // must produce an event from both, proving the first watcher was not dropped.
    #[tokio::test]
    async fn test_watch_multiple_paths_delivers_events_from_all() {
        let first_dir = TempDir::new().unwrap();
        let second_dir = TempDir::new().unwrap();
        let watcher = FileWatcher::new(WatcherConfig::default()).unwrap();
        let mut rx = watcher.subscribe();

        watcher.watch(first_dir.path()).unwrap();
        watcher.watch(second_dir.path()).unwrap();

        assert_eq!(watcher.watched_paths().len(), 2);
        assert!(watcher.is_watching(first_dir.path()));
        assert!(watcher.is_watching(second_dir.path()));

        let first_file = first_dir.path().join("first.txt");
        let second_file = second_dir.path().join("second.txt");
        fs::write(&first_file, "one").unwrap();
        fs::write(&second_file, "two").unwrap();

        let mut saw_first = false;
        let mut saw_second = false;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(1_000);
        while let Ok(Ok(event)) = tokio::time::timeout_at(deadline, rx.recv()).await {
            if event.file == first_file {
                saw_first = true;
            }
            if event.file == second_file {
                saw_second = true;
            }
            if saw_first && saw_second {
                break;
            }
        }

        assert!(saw_first, "first path must keep being monitored");
        assert!(saw_second, "second path must be monitored");
    }

    // Test basis: M-63. Oracle: after unwatch() the path must be removed from
    // the registry AND detached from the backend, so no further event arrives.
    #[tokio::test]
    async fn test_unwatch_detaches_path_from_backend_watcher() {
        let temp_dir = TempDir::new().unwrap();
        let watcher = FileWatcher::new(WatcherConfig::default()).unwrap();
        let mut rx = watcher.subscribe();

        watcher.watch(temp_dir.path()).unwrap();

        let before = temp_dir.path().join("before.txt");
        fs::write(&before, "before").unwrap();
        let baseline = tokio::time::timeout(Duration::from_millis(1_000), rx.recv()).await;
        assert!(
            matches!(baseline, Ok(Ok(_))),
            "event before unwatch must arrive"
        );

        watcher.unwatch(temp_dir.path()).unwrap();
        assert!(!watcher.is_watching(temp_dir.path()));
        assert!(watcher.watched_paths().is_empty());

        // Drain events queued before the unwatch took effect.
        while rx.try_recv().is_ok() {}

        let after = temp_dir.path().join("after.txt");
        fs::write(&after, "after").unwrap();

        let result = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
        assert!(result.is_err(), "no event must be delivered after unwatch");
    }

    // Test basis: M-63. Oracle: the second watched root must also use recursive
    // mode, so a file several levels below it still produces an event.
    #[tokio::test]
    async fn test_recursive_mode_applies_to_later_paths_and_deep_hierarchy() {
        let first_dir = TempDir::new().unwrap();
        let second_dir = TempDir::new().unwrap();
        let watcher = FileWatcher::new(WatcherConfig::default()).unwrap();
        let mut rx = watcher.subscribe();

        watcher.watch(first_dir.path()).unwrap();
        watcher.watch(second_dir.path()).unwrap();

        let nested = second_dir
            .path()
            .join("level1")
            .join("level2")
            .join("level3");
        fs::create_dir_all(&nested).unwrap();
        // Let the backend attach watches to the newly created subdirectories.
        sleep(Duration::from_millis(100)).await;

        let deep_file = nested.join("deep.txt");
        fs::write(&deep_file, "deep").unwrap();
        // A second write (outside the default 100ms debounce window) gives the
        // backend another chance if the first raced with sub-watch attachment.
        sleep(Duration::from_millis(120)).await;
        fs::write(&deep_file, "deep again").unwrap();

        let mut saw_deep = false;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(1_000);
        loop {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Ok(event)) if event.file == deep_file => {
                    saw_deep = true;
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) | Err(_) => break,
            }
        }

        assert!(
            saw_deep,
            "second path must also be watched recursively (deep file)"
        );
    }
}

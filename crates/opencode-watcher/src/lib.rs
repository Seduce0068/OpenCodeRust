//! File watcher foundation: path registration, debounced event coalescing and
//! broadcast delivery of file change events.
//!
//! # 現状の配線状況
//!
//! 現状は未配線（workspace member のみ）。ルート `Cargo.toml` の workspace
//! members に列挙されているだけで、他のクレートから依存されていないため、
//! 現行の実行系からは利用されない。この状態は意図的なもので、配線は別途の
//! 作業として扱う。本次変更ではコードの挙動を一切変更しない。

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

        // Keep the backend result instead of propagating it with `?` right away.
        // Registry/debounce cleanup and watcher teardown must run even when the
        // backend fails to detach the path; otherwise is_watching() stays true
        // and stale debounce keys survive (WATCHER-UNWATCH-PARTIAL).
        let backend_result: Result<(), WatcherError> = match watcher_guard.as_mut() {
            Some(watcher) => watcher
                .unwatch(path)
                .map_err(|e| WatcherError::WatchError(e.to_string())),
            None => Ok(()),
        };

        self.watched_paths.remove(path);
        // Debounce bookkeeping is keyed by the concrete file paths that fired
        // events, not by the watched root. Drop every entry under `path` so an
        // unwatched subtree cannot leave stale keys in the map forever.
        self.clear_debounce_entries_for(path);

        if self.watched_paths.is_empty() {
            *watcher_guard = None;
            info!("Stopped all watchers");
        } else {
            info!(path = ?path, "Stopped watching directory");
        }

        // The preserved backend error (if any) is returned only after cleanup.
        backend_result
    }

    /// すべての監視を停止し、登録済みパスとデバウンス情報を破棄する。
    pub fn stop(&self) {
        self.watched_paths.clear();
        *self.watcher.write() = None;
        self.event_last_seen.clear();
        info!("Stopped all file watchers");
    }

    /// Removes debounce bookkeeping for `path` and every path beneath it.
    ///
    /// Event callbacks record the concrete changed file, so a single watched
    /// directory can leave many child entries in `event_last_seen`. Matching
    /// by path prefix (component-wise via [`Path::starts_with`]) clears the
    /// whole subtree without touching other watched roots.
    fn clear_debounce_entries_for(&self, path: &Path) {
        self.event_last_seen
            .retain(|entry_path, _| !entry_path.starts_with(path));
    }

    /// 現在登録されている監視パスの一覧を返す。
    pub fn watched_paths(&self) -> Vec<PathBuf> {
        self.watched_paths.iter().map(|p| p.clone()).collect()
    }

    /// 指定パスが現在監視対象かどうかを返す。
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

/// プロセス共有の既定設定 `FileWatcher` を取得する（初回呼び出しで生成）。
pub fn get_watcher() -> Arc<FileWatcher> {
    FILE_WATCHER
        .get_or_init(|| {
            Arc::new(
                FileWatcher::new(WatcherConfig::default()).expect("Failed to create file watcher"),
            )
        })
        .clone()
}

/// 指定設定で共有 `FileWatcher` を初期化して返す（既に初期化済みなら既存を返す）。
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

        // 固定 sleep + try_recv の Empty 無視ではイベント未発行でも合格してしまう。
        // deadline 付きで 1 件の受信を必須化し、同ファイルの他テストと同じ方式に揃える。
        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("timed out waiting for watch event")
            .expect("watch event channel closed unexpectedly");
        assert!(event.file.ends_with("test.txt"));
        assert_eq!(event.event, FileEvent::Add);
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

        // Drain repeatedly until the queue is empty: callbacks already in
        // flight when unwatch() returned may still enqueue pre-unwatch events.
        // Bounded passes with a short gap avoid both a single-pass miss and an
        // unbounded wait.
        for _ in 0..5 {
            let mut drained_any = false;
            while rx.try_recv().is_ok() {
                drained_any = true;
            }
            if !drained_any {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }

        let after = temp_dir.path().join("after.txt");
        fs::write(&after, "after").unwrap();

        // Replace the fixed 300ms single timeout with several bounded checks
        // over a 500ms window: a late event is still caught, and the upper
        // bound keeps the test from hanging.
        let monitor_deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        while tokio::time::Instant::now() < monitor_deadline {
            let check_until =
                (tokio::time::Instant::now() + Duration::from_millis(100)).min(monitor_deadline);
            match tokio::time::timeout_at(check_until, rx.recv()).await {
                // unwatch 前に enqueue された in-flight イベントは無視し、
                // 監視対象の after.txt が通知された場合のみ失敗とする。
                Ok(Ok(event)) if event.file == after => {
                    panic!("no event for the newly written file must be delivered after unwatch")
                }
                Ok(Ok(_)) => {}
                Ok(Err(_)) => break,
                Err(_) => {}
            }
        }
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

        let deep_file = nested.join("deep.txt");
        let mut saw_deep = false;
        // No fixed sleeps: retry the write within the original 1000ms budget.
        // A sub-watch can attach late, so each retry is a fresh chance; the
        // 150ms step stays outside the 100ms debounce/poll window so a write
        // can surface as a forwarded event. Sleep until the step boundary in
        // case the queue closes or floods, so the loop always makes progress.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(1_000);
        let retry_step = Duration::from_millis(150);
        while tokio::time::Instant::now() < deadline {
            fs::write(&deep_file, "deep").unwrap();
            let wait_until = (tokio::time::Instant::now() + retry_step).min(deadline);
            loop {
                match tokio::time::timeout_at(wait_until, rx.recv()).await {
                    Ok(Ok(event)) if event.file == deep_file => {
                        saw_deep = true;
                        break;
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(_)) | Err(_) => break,
                }
            }
            if saw_deep {
                break;
            }
            tokio::time::sleep_until(wait_until).await;
        }

        assert!(
            saw_deep,
            "second path must also be watched recursively (deep file)"
        );
    }

    // Regression: unwatch() must drop debounce bookkeeping for the removed root
    // and everything beneath it, otherwise those keys live forever.
    #[test]
    fn test_unwatch_clears_debounce_entries_for_path_and_descendants() {
        let temp_dir = TempDir::new().unwrap();
        let watcher = FileWatcher::new(WatcherConfig::default()).unwrap();
        watcher.watch(temp_dir.path()).unwrap();

        let direct = temp_dir.path().join("direct.txt");
        let nested = temp_dir.path().join("sub").join("nested.txt");
        let unrelated = PathBuf::from("/unrelated/keep.txt");

        watcher
            .event_last_seen
            .insert(direct.clone(), Instant::now());
        watcher
            .event_last_seen
            .insert(nested.clone(), Instant::now());
        watcher
            .event_last_seen
            .insert(unrelated.clone(), Instant::now());
        assert_eq!(watcher.event_last_seen.len(), 3);

        watcher.unwatch(temp_dir.path()).unwrap();

        assert!(!watcher.event_last_seen.contains_key(&direct));
        assert!(!watcher.event_last_seen.contains_key(&nested));
        assert!(
            watcher.event_last_seen.contains_key(&unrelated),
            "entries outside the unwatched root must be preserved"
        );
    }

    // Regression: stop() must clear all debounce bookkeeping.
    #[test]
    fn test_stop_clears_all_debounce_entries() {
        let watcher = FileWatcher::new(WatcherConfig::default()).unwrap();
        watcher
            .event_last_seen
            .insert(PathBuf::from("/any/path.txt"), Instant::now());
        assert_eq!(watcher.event_last_seen.len(), 1);

        watcher.stop();

        assert!(
            watcher.event_last_seen.is_empty(),
            "stop() must drop every debounce entry"
        );
    }

    // Regression: WATCHER-UNWATCH-PARTIAL. A failing backend unwatch() must not
    // leave the path registered or its debounce keys behind. The backend is
    // forced to fail by unwatching a registry-only path it never watched
    // (notify::Error::watch_not_found).
    #[test]
    fn test_unwatch_backend_error_still_clears_registry_and_debounce() {
        let temp_dir = TempDir::new().unwrap();
        let watcher = FileWatcher::new(WatcherConfig::default()).unwrap();
        // Lazily create the shared backend with a real watched root.
        watcher.watch(temp_dir.path()).unwrap();

        // Phantom subtree: registered and debounce-tracked, but never passed to
        // the backend, so backend unwatch() fails and returns an error.
        let phantom_root = temp_dir.path().join("never_watched");
        let debounce_key = phantom_root.join("stale.txt");
        watcher.watched_paths.insert(phantom_root.clone());
        watcher
            .event_last_seen
            .insert(debounce_key.clone(), Instant::now());
        assert!(watcher.is_watching(&phantom_root));
        assert!(watcher.event_last_seen.contains_key(&debounce_key));

        let result = watcher.unwatch(&phantom_root);
        assert!(
            matches!(result, Err(WatcherError::WatchError(_))),
            "backend unwatch must surface its error"
        );

        // Cleanup must have run despite the error.
        assert!(
            !watcher.is_watching(&phantom_root),
            "path must be unregistered even when backend unwatch fails"
        );
        assert!(
            !watcher.event_last_seen.contains_key(&debounce_key),
            "debounce keys under the path must be cleared even when unwatch fails"
        );
        // The genuinely watched root is left intact.
        assert!(watcher.is_watching(temp_dir.path()));
    }

    // Regression: WATCHER-UNWATCH-PARTIAL. When the failing path is the last
    // registered one, the backend watcher must still be torn down.
    #[test]
    fn test_unwatch_backend_error_drops_watcher_when_last_path_removed() {
        let temp_dir = TempDir::new().unwrap();
        let watcher = FileWatcher::new(WatcherConfig::default()).unwrap();
        watcher.watch(temp_dir.path()).unwrap();
        assert!(watcher.watcher.read().is_some());

        // Leave only a phantom registry entry whose backend unwatch will fail.
        let phantom = temp_dir.path().join("never_watched");
        watcher.watched_paths.clear();
        watcher.watched_paths.insert(phantom.clone());

        let result = watcher.unwatch(&phantom);
        assert!(matches!(result, Err(WatcherError::WatchError(_))));
        assert!(watcher.watched_paths.is_empty());
        assert!(
            watcher.watcher.read().is_none(),
            "backend watcher must be dropped when the last registered path is removed"
        );
    }
}

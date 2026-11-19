use chrono::Local;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn from_str(s: &str) -> Self {
        match s.to_uppercase().as_str() {
            "DEBUG" => LogLevel::Debug,
            "INFO" => LogLevel::Info,
            "WARN" => LogLevel::Warn,
            "ERROR" => LogLevel::Error,
            _ => LogLevel::Info,
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogLevel::Debug => write!(f, "DEBUG"),
            LogLevel::Info => write!(f, "INFO"),
            LogLevel::Warn => write!(f, "WARN"),
            LogLevel::Error => write!(f, "ERROR"),
        }
    }
}

static LAST_TIMESTAMP: Mutex<i64> = Mutex::new(0);
static LOGGERS: LazyLock<Mutex<HashMap<String, CachedLogger>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// 既定のログディレクトリを返す。
///
/// Unix では他人が監視しやすいカレントディレクトリを避け、ユーザー専用の
/// 状態ディレクトリ（`XDG_STATE_HOME` または `$HOME/.local/state`）配下に
/// ログを置く。Windows は `cfg` 分岐で従来どおりカレントディレクトリを使用する。
#[cfg(unix)]
fn default_log_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir).join("opencode").join("logs");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("opencode")
                .join("logs");
        }
    }
    // HOME が無い環境でもカレント直下の専用サブディレクトリに閉じ込める。
    PathBuf::from(".").join(".opencode").join("logs")
}

#[cfg(not(unix))]
fn default_log_dir() -> PathBuf {
    PathBuf::from(".")
}

/// ログファイルを安全な権限で開く。
///
/// Unix では新規作成時に `0600` を指定し、加えて既存ファイルが `0644` 等で
/// 作られていた場合も `set_permissions` で `0600` に是正する。
/// 権限を設定できない場合はエラーとして返し、呼び出し側で stderr へ
/// フォールバックさせる（安全側に倒す）。
#[cfg(unix)]
fn open_log_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(not(unix))]
fn open_log_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

/// タグ・extra・メッセージをログ行に埋め込む前に制御文字をエスケープする。
///
/// 改行や ANSI エスケープをそのまま書き出すと、攻撃者が偽のログ行や
/// 端末制御シーケンスを注入できる。バックスラッシュを最初に倍増させ、
/// 入力中の `\\n` などがエスケープ表現と衝突しないようにする。
fn escape_log_field(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{2028}' | '\u{2029}' => {
                escaped.push_str(&format!("\\u{{{:04x}}}", ch as u32));
            }
            c if c.is_control() => {
                // C0 / C1 / DEL を含む制御文字は可視化して注入を無効化する。
                escaped.push_str(&format!("\\u{{{:04x}}}", c as u32));
            }
            c => escaped.push(c),
        }
    }
    escaped
}

struct CachedLogger {
    tags: HashMap<String, String>,
    level: LogLevel,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
}

pub struct Logger {
    tags: HashMap<String, String>,
    level: LogLevel,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Clone for Logger {
    fn clone(&self) -> Self {
        Self {
            tags: self.tags.clone(),
            level: self.level,
            writer: Arc::clone(&self.writer),
        }
    }
}

impl Logger {
    fn new(tags: HashMap<String, String>, writer: Arc<Mutex<Box<dyn Write + Send>>>) -> Self {
        let level = tags
            .get("level")
            .map(|l| LogLevel::from_str(l))
            .unwrap_or(LogLevel::Info);
        Self {
            tags,
            level,
            writer,
        }
    }

    fn should_log(&self, level: LogLevel) -> bool {
        let priority = match level {
            LogLevel::Debug => 0,
            LogLevel::Info => 1,
            LogLevel::Warn => 2,
            LogLevel::Error => 3,
        };
        let my_priority = match self.level {
            LogLevel::Debug => 0,
            LogLevel::Info => 1,
            LogLevel::Warn => 2,
            LogLevel::Error => 3,
        };
        priority >= my_priority
    }

    fn build_message(
        &self,
        level: LogLevel,
        message: &str,
        extra: Option<&HashMap<String, String>>,
    ) -> String {
        let mut last = LAST_TIMESTAMP.lock().unwrap();
        let now = Local::now();
        let timestamp = now.format("%Y-%m-%dT%H:%M:%S").to_string();
        let diff = now.timestamp_millis() - *last;
        *last = now.timestamp_millis();
        drop(last);

        let mut parts = vec![timestamp, format!("+{}ms", diff)];

        for (key, value) in &self.tags {
            parts.push(format!(
                "{}={}",
                escape_log_field(key),
                escape_log_field(value)
            ));
        }

        if let Some(extra) = extra {
            for (key, value) in extra {
                parts.push(format!(
                    "{}={}",
                    escape_log_field(key),
                    escape_log_field(value)
                ));
            }
        }

        parts.push(escape_log_field(message));

        format!("{} {}\n", level, parts.join(" "))
    }

    pub fn debug(&self, message: &str, extra: Option<HashMap<String, String>>) {
        if self.should_log(LogLevel::Debug) {
            let msg = self.build_message(LogLevel::Debug, message, extra.as_ref());
            if let Ok(mut writer) = self.writer.lock() {
                let _ = writer.write_all(msg.as_bytes());
                let _ = writer.flush();
            }
        }
    }

    pub fn info(&self, message: &str, extra: Option<HashMap<String, String>>) {
        if self.should_log(LogLevel::Info) {
            let msg = self.build_message(LogLevel::Info, message, extra.as_ref());
            if let Ok(mut writer) = self.writer.lock() {
                let _ = writer.write_all(msg.as_bytes());
                let _ = writer.flush();
            }
        }
    }

    pub fn warn(&self, message: &str, extra: Option<HashMap<String, String>>) {
        if self.should_log(LogLevel::Warn) {
            let msg = self.build_message(LogLevel::Warn, message, extra.as_ref());
            if let Ok(mut writer) = self.writer.lock() {
                let _ = writer.write_all(msg.as_bytes());
                let _ = writer.flush();
            }
        }
    }

    pub fn error(&self, message: &str, extra: Option<HashMap<String, String>>) {
        if self.should_log(LogLevel::Error) {
            let msg = self.build_message(LogLevel::Error, message, extra.as_ref());
            if let Ok(mut writer) = self.writer.lock() {
                let _ = writer.write_all(msg.as_bytes());
                let _ = writer.flush();
            }
        }
    }

    pub fn tag(&self, key: &str, value: &str) -> TaggedLogger {
        let mut tags = self.tags.clone();
        tags.insert(key.to_string(), value.to_string());
        TaggedLogger {
            tags,
            writer: Arc::clone(&self.writer),
            level: self.level,
        }
    }
}

pub struct TaggedLogger {
    tags: HashMap<String, String>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    level: LogLevel,
}

impl Clone for TaggedLogger {
    fn clone(&self) -> Self {
        Self {
            tags: self.tags.clone(),
            writer: Arc::clone(&self.writer),
            level: self.level,
        }
    }
}

impl TaggedLogger {
    pub fn debug(&self, message: &str, extra: Option<HashMap<String, String>>) {
        if self.should_log(LogLevel::Debug) {
            let msg = self.build_message(LogLevel::Debug, message, extra.as_ref());
            if let Ok(mut writer) = self.writer.lock() {
                let _ = writer.write_all(msg.as_bytes());
                let _ = writer.flush();
            }
        }
    }

    pub fn info(&self, message: &str, extra: Option<HashMap<String, String>>) {
        if self.should_log(LogLevel::Info) {
            let msg = self.build_message(LogLevel::Info, message, extra.as_ref());
            if let Ok(mut writer) = self.writer.lock() {
                let _ = writer.write_all(msg.as_bytes());
                let _ = writer.flush();
            }
        }
    }

    pub fn warn(&self, message: &str, extra: Option<HashMap<String, String>>) {
        if self.should_log(LogLevel::Warn) {
            let msg = self.build_message(LogLevel::Warn, message, extra.as_ref());
            if let Ok(mut writer) = self.writer.lock() {
                let _ = writer.write_all(msg.as_bytes());
                let _ = writer.flush();
            }
        }
    }

    pub fn error(&self, message: &str, extra: Option<HashMap<String, String>>) {
        if self.should_log(LogLevel::Error) {
            let msg = self.build_message(LogLevel::Error, message, extra.as_ref());
            if let Ok(mut writer) = self.writer.lock() {
                let _ = writer.write_all(msg.as_bytes());
                let _ = writer.flush();
            }
        }
    }

    fn should_log(&self, level: LogLevel) -> bool {
        let priority = match level {
            LogLevel::Debug => 0,
            LogLevel::Info => 1,
            LogLevel::Warn => 2,
            LogLevel::Error => 3,
        };
        let my_priority = match self.level {
            LogLevel::Debug => 0,
            LogLevel::Info => 1,
            LogLevel::Warn => 2,
            LogLevel::Error => 3,
        };
        priority >= my_priority
    }

    fn build_message(
        &self,
        level: LogLevel,
        message: &str,
        extra: Option<&HashMap<String, String>>,
    ) -> String {
        let mut last = LAST_TIMESTAMP.lock().unwrap();
        let now = Local::now();
        let timestamp = now.format("%Y-%m-%dT%H:%M:%S").to_string();
        let diff = now.timestamp_millis() - *last;
        *last = now.timestamp_millis();
        drop(last);

        let mut parts = vec![timestamp, format!("+{}ms", diff)];

        for (key, value) in &self.tags {
            parts.push(format!(
                "{}={}",
                escape_log_field(key),
                escape_log_field(value)
            ));
        }

        if let Some(extra) = extra {
            for (key, value) in extra {
                parts.push(format!(
                    "{}={}",
                    escape_log_field(key),
                    escape_log_field(value)
                ));
            }
        }

        parts.push(escape_log_field(message));

        format!("{} {}\n", level, parts.join(" "))
    }
}

pub struct Log;

impl Log {
    pub fn create(tags: Option<HashMap<String, String>>) -> Logger {
        let tags = tags.unwrap_or_default();

        if let Some(service) = tags.get("service") {
            if let Ok(loggers) = LOGGERS.lock() {
                if let Some(cached) = loggers.get(service) {
                    return Logger {
                        tags: cached.tags.clone(),
                        level: cached.level,
                        writer: Arc::clone(&cached.writer),
                    };
                }
            }
        }

        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(std::io::stderr())));
        let logger = Logger::new(tags.clone(), Arc::clone(&writer));

        if let Some(service) = tags.get("service") {
            if let Ok(mut loggers) = LOGGERS.lock() {
                loggers.insert(
                    service.to_string(),
                    CachedLogger {
                        tags: tags.clone(),
                        level: logger.level,
                        writer: Arc::clone(&writer),
                    },
                );
            }
        }

        logger
    }

    pub fn default_logger() -> Logger {
        let mut tags = HashMap::new();
        tags.insert("service".to_string(), "default".to_string());
        Self::create(Some(tags))
    }

    pub fn init(
        _level: Option<LogLevel>,
        log_dir: Option<PathBuf>,
        print: bool,
    ) -> Option<PathBuf> {
        if print {
            return None;
        }

        let log_dir = log_dir.unwrap_or_else(default_log_dir);
        let timestamp = Local::now().format("%Y-%m-%dT%H%M%S").to_string();
        let log_path = log_dir.join(format!("{}.log", timestamp));

        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let file = open_log_file(&log_path).ok();

        if let Some(file) = file {
            let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(Box::new(file)));
            if let Ok(mut loggers) = LOGGERS.lock() {
                for cached in loggers.values_mut() {
                    cached.writer = Arc::clone(&writer);
                }
            }
        }

        Some(log_path)
    }
}

/// 明示指定されたログレベルを `EnvFilter` へ反映する。
///
/// `level` が `Some` で空でない場合はその値をディレクティブとして使用し、
/// それ以外は従来どおり `RUST_LOG` → `info` の順でフォールバックする。
fn build_filter(level: Option<&str>) -> EnvFilter {
    match level {
        Some(level) if !level.trim().is_empty() => EnvFilter::new(level),
        _ => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    }
}

/// `print = false` 経路では `WorkerGuard` を返す。呼び出し側は `main` の
/// ローカル束縛として保持し、スコープ終了時に drop させて末尾ログをフラッシュする。
/// 二重初期化を避けるため `try_init` を使い、既存 subscriber がある場合は
/// そちらへログを流す（tracing が no-op にはならない）。
pub fn init_tracing(
    level: Option<&str>,
    log_dir: Option<PathBuf>,
    print: bool,
) -> (
    Option<PathBuf>,
    Option<tracing_appender::non_blocking::WorkerGuard>,
) {
    if print {
        let filter = build_filter(level);
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer())
            .try_init();
        return (None, None);
    }

    let log_dir = log_dir.unwrap_or_else(default_log_dir);
    let timestamp = Local::now().format("%Y-%m-%dT%H%M%S").to_string();
    let log_path = log_dir.join(format!("{}.log", timestamp));

    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    match open_log_file(&log_path) {
        Ok(file) => {
            let (non_blocking, guard) = tracing_appender::non_blocking(file);
            let filter = build_filter(level);
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().with_writer(non_blocking))
                .try_init();
            (Some(log_path), Some(guard))
        }
        Err(_) => {
            // ファイルを開けない場合でも tracing が no-op にならないよう、
            // print=true 経路と同じ stderr の fmt::layer を必ず設定する。
            let filter = build_filter(level);
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().with_writer(std::io::stderr))
                .try_init();
            (Some(log_path), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 環境変数を書き換えずに、フォールバック経路の期待値を組み立てる。
    ///
    /// `RUST_LOG` の変更は並列実行中の他テストと衝突しうるため、ここでは
    /// 現在値を読み取るだけにとどめ、`build_filter` と同じ規則
    /// （未設定・不正値なら `info`）で期待値を導出する。
    fn fallback_directive() -> String {
        match std::env::var("RUST_LOG") {
            Ok(value) => EnvFilter::try_new(&value)
                .map(|filter| filter.to_string())
                .unwrap_or_else(|_| "info".to_string()),
            Err(_) => "info".to_string(),
        }
    }

    #[test]
    fn build_filter_prefers_explicit_level_over_rust_log() {
        let fallback = fallback_directive();
        // 環境値と必ず異なる比較対象を選び、優先順位を明示的に検証する。
        let explicit = if fallback == "debug" {
            "trace"
        } else {
            "debug"
        };

        assert_ne!(fallback, explicit);
        assert_eq!(build_filter(Some(explicit)).to_string(), explicit);
        assert_eq!(build_filter(None).to_string(), fallback);
    }

    #[test]
    fn build_filter_accepts_uppercase_level() {
        assert_eq!(build_filter(Some("ERROR")).to_string(), "error");
    }

    #[test]
    fn build_filter_falls_back_when_level_is_empty() {
        let expected = fallback_directive();

        assert_eq!(build_filter(Some("")).to_string(), expected);
        assert_eq!(build_filter(Some("   ")).to_string(), expected);
        assert_eq!(build_filter(None).to_string(), expected);
    }

    #[test]
    fn escape_log_field_neutralizes_newlines_and_control_chars() {
        let escaped = escape_log_field("a\nb\r\nc\td\u{1b}[31m\\n\u{0}\u{2028}");
        assert_eq!(
            escaped,
            "a\\nb\\r\\nc\\td\\u{001b}[31m\\\\n\\u{0000}\\u{2028}"
        );
        assert!(!escaped.contains('\n'));
        assert!(!escaped.contains('\r'));
        assert!(!escaped.contains('\u{1b}'));
    }

    #[test]
    fn build_message_escapes_tags_extra_and_message() {
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(Box::new(Vec::new())));
        let mut tags = HashMap::new();
        tags.insert("service".to_string(), "svc\nINFO forged".to_string());
        tags.insert("ctl\u{7f}".to_string(), "v\r\nx".to_string());
        let logger = Logger::new(tags, writer);

        let mut extra = HashMap::new();
        extra.insert("extra\u{1b}".to_string(), "a\tb".to_string());

        let line = logger.build_message(LogLevel::Info, "hello\nworld", Some(&extra));

        assert!(!line.contains("svc\nINFO"));
        assert!(!line.contains("hello\nworld"));
        assert!(line.contains("service=svc\\nINFO forged"));
        assert!(line.contains("hello\\nworld"));
        assert!(line.contains("ctl\\u{007f}=v\\r\\nx"));
        assert!(line.contains("extra\\u{001b}=a\\tb"));
        // 注入された改行が無く、行終端の改行だけが残る。
        assert_eq!(line.lines().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn open_log_file_creates_0600_and_fixes_existing_0644() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("opencode-util-log-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.log");

        {
            let file = open_log_file(&path).unwrap();
            drop(file);
        }
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "新規ログファイルは 0600 で作成されるべき");

        // 過去に 0644 で作られたファイルも再オープン時に 0600 へ是正される。
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        {
            let _file = open_log_file(&path).unwrap();
        }
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "既存ファイルも 0600 に是正されるべき");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn default_log_dir_is_user_scoped_when_home_is_available() {
        let has_xdg = std::env::var("XDG_STATE_HOME")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        let has_home = std::env::var("HOME")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        if !has_xdg && !has_home {
            return; // 環境変数が無い環境では既定値を検証できない
        }

        let dir = default_log_dir();
        assert_ne!(dir, PathBuf::from("."));
        assert!(dir.ends_with("logs"));
    }

    #[test]
    fn init_tracing_initializes_stderr_fallback_when_file_open_fails() {
        // 既存ファイルを log_dir として渡すとログファイルの open は ENOTDIR で
        // 失敗する。その場合も subscriber が設定され、tracing が no-op に
        // ならないことを検証する。
        let blocker =
            std::env::temp_dir().join(format!("opencode-util-fallback-{}", std::process::id()));
        std::fs::write(&blocker, b"not a directory").unwrap();

        let (log_path, guard) = init_tracing(Some("info"), Some(blocker.clone()), false);
        let _ = std::fs::remove_file(&blocker);

        assert!(log_path.is_some());
        assert!(guard.is_none());
        // フォールバックで fmt::layer 付き subscriber が init されていれば、
        // 後続の try_init は失敗する（= 何も設定されない no-op ではない）。
        assert!(
            tracing_subscriber::registry().try_init().is_err(),
            "ファイル失敗時に stderr フォールバック subscriber が初期化されていない"
        );
    }
}

use chrono::{Local, SecondsFormat};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;
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

// LAST_TIMESTAMP はロガー単位の `Arc<Mutex<i64>>` へ移行した（下記 struct を参照）。
// グローバル共有すると他スレッド／他ロガーの前回時刻を差分の基準にしてしまい、
// `+NNms` が他人の時刻として誤読されるため。
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

/// 既定のログ保持世代数（現在のログ 1 世代 + 過去 6 世代）。
const DEFAULT_LOG_RETENTION: usize = 7;

/// 保持世代数を上書きする環境変数名（設定ファイル・CLI はスコープ外）。
const LOG_RETENTION_ENV: &str = "OPENCODE_LOG_RETENTION";

/// 環境変数の文字列を保持世代数として解釈する。正の整数のみ有効とする。
fn parse_log_retention(raw: &str) -> Option<usize> {
    raw.trim().parse::<usize>().ok().filter(|&n| n > 0)
}

/// 保持世代数を返す。未設定・不正値の場合は既定値へフォールバックする。
fn log_retention() -> usize {
    std::env::var(LOG_RETENTION_ENV)
        .ok()
        .and_then(|value| parse_log_retention(&value))
        .unwrap_or(DEFAULT_LOG_RETENTION)
}

/// ログファイル名を生成する。
///
/// 既存の `YYYY-MM-DDTHHMMSS.log` 形式を利用する利用者との後方互換を保つため、
/// タイムスタンプ部分はそのままに `-<pid>-<nanos>` をサフィックスとして付ける。
/// PID とナノ秒の両方を入れることで、同一秒の同時起動に加えて PID 再利用による
/// 衝突も回避し、`open_log_file` の `truncate(true)` が他プロセスのログを
/// 破壊しないようにする。
fn log_file_name(timestamp: &str, pid: u32, nanos: u32) -> String {
    format!("{timestamp}-{pid}-{nanos:09}.log")
}

/// ローテーション対象のログファイル名かどうかを判定する。
///
/// 先頭 17 文字が `YYYY-MM-DDTHHMMSS` で、残りが `-<pid>-<nanos>` または
/// 旧形式（サフィックスなし）の `.log` ファイルだけを対象にする。
fn is_rotated_log_name(name: &str) -> bool {
    const STAMP_LEN: usize = 17; // YYYY-MM-DDTHHMMSS
    let Some(stem) = name.strip_suffix(".log") else {
        return false;
    };
    let Some(stamp) = stem.get(..STAMP_LEN) else {
        return false;
    };
    let bytes = stamp.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' {
        return false;
    }
    const DIGIT_POSITIONS: [usize; 14] = [0, 1, 2, 3, 5, 6, 8, 9, 11, 12, 13, 14, 15, 16];
    if !DIGIT_POSITIONS.iter().all(|&i| bytes[i].is_ascii_digit()) {
        return false;
    }
    match stem.get(STAMP_LEN..) {
        // 旧形式も保持対象に含め、過去のログが無限に蓄積するのを止める。
        Some("") => true,
        Some(suffix) if suffix.starts_with('-') => {
            // 厳密に -<digits>-<digits> のみを管理対象とする。空や片側だけの
            // サフィックスを持つ無関係な .log を誤削除しない。
            let mut parts = suffix[1..].split('-');
            match (parts.next(), parts.next(), parts.next()) {
                (Some(pid), Some(nanos), None) => {
                    !pid.is_empty()
                        && pid.bytes().all(|b| b.is_ascii_digit())
                        && !nanos.is_empty()
                        && nanos.bytes().all(|b| b.is_ascii_digit())
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// 保持世代数を超える古いログを削除し、削除したファイル数を返す。
///
/// 生成順は mtime で判定する。ファイル名は `<timestamp>-<pid>-<nanos>` で
/// PID がナノ秒より前に来るため辞書順では新しいログが先に並ぶことがある。
/// `keep` には「残す既存ファイル数」を渡す（呼び出し側で新規 1 世代分を引く）。
///
/// `tracing_appender::rolling::RollingFileAppender::builder()` の
/// `max_log_files` も検討したが、内部で `append(true).create(true)` のみで
/// ファイルを開くため `0600` 要件と衝突し、生成名に PID/ナノ秒を埋め込めない。
/// よって既存の `tracing_appender::non_blocking` はそのまま再利用し、
/// ローテーションと保持は `std::fs` で行う（新規依存なし）。
fn prune_old_logs(dir: &Path, keep: usize) -> std::io::Result<usize> {
    let mut logs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_rotated_log_name(name) {
            logs.push(entry.path());
        }
    }
    logs.sort_by_key(|path| {
        let modified = std::fs::metadata(path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        // mtime の分解能では同一秒を区別できない環境向けの安定順序。
        (modified, path.file_name().map(|name| name.to_owned()))
    });

    let removable = logs.len().saturating_sub(keep);
    let mut removed = 0;
    for path in logs.into_iter().take(removable) {
        if std::fs::remove_file(path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// 新しいログファイルのパスを決め、親ディレクトリを用意し、保持世代数を
/// 超える旧ログを削除する。ファイル自体はここでは作成しないため、
/// `open_log_file` の `0600` 保証と stderr フォールバックは従来どおり。
fn prepare_log_path(log_dir: &Path) -> PathBuf {
    let now = Local::now();
    let timestamp = now.format("%Y-%m-%dT%H%M%S").to_string();
    let log_path = log_dir.join(log_file_name(
        &timestamp,
        std::process::id(),
        now.timestamp_subsec_nanos(),
    ));

    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // 新規ファイル生成前に削除するので、このパスが削除対象になることはない。
    let _ = prune_old_logs(log_dir, log_retention().saturating_sub(1));

    log_path
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

/// RFC3339（UTC オフセット付き・ミリ秒）のタイムスタンプと、同一ロガー内での
/// 前回ログからの経過ミリ秒を返す。
///
/// `last_timestamp` はロガー単位で保持するため、他スレッド／他ロガーの時刻を
/// 基準にした誤読が起きない。ロックは `into_inner()` でポイズンから回復する。
fn format_timestamp(last_timestamp: &Mutex<i64>) -> (String, i64) {
    // ロック取得後に現在時刻を取り、clone 間で順序が入れ替わっても
    // 過去へ巻き戻さないよう max で更新する。
    let mut last = last_timestamp
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let now = Local::now();
    let timestamp = now.to_rfc3339_opts(SecondsFormat::Millis, false);
    let now_ms = now.timestamp_millis();

    // 初回（0 は未初期化sentinel）は差分 0 とし、epoch 全体を差分としない。
    let diff = if *last == 0 { 0 } else { now_ms - *last };
    *last = (*last).max(now_ms);

    (timestamp, diff)
}

struct CachedLogger {
    tags: HashMap<String, String>,
    level: LogLevel,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// 直前の絶対時刻（epoch millis）。ロガーごとに保持する。
    last_timestamp: Arc<Mutex<i64>>,
}

pub struct Logger {
    tags: HashMap<String, String>,
    level: LogLevel,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// 直前の絶対時刻（epoch millis）。clone 間では共有するが他ロガーとは独立。
    last_timestamp: Arc<Mutex<i64>>,
}

impl Clone for Logger {
    fn clone(&self) -> Self {
        Self {
            tags: self.tags.clone(),
            level: self.level,
            writer: Arc::clone(&self.writer),
            last_timestamp: Arc::clone(&self.last_timestamp),
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
            last_timestamp: Arc::new(Mutex::new(0)),
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
        let (timestamp, diff) = format_timestamp(&self.last_timestamp);
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
            last_timestamp: Arc::clone(&self.last_timestamp),
        }
    }
}

pub struct TaggedLogger {
    tags: HashMap<String, String>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    level: LogLevel,
    /// 元の Logger と同じ時刻系を共有する（タグ違いで差分が壊れないように）。
    last_timestamp: Arc<Mutex<i64>>,
}

impl Clone for TaggedLogger {
    fn clone(&self) -> Self {
        Self {
            tags: self.tags.clone(),
            writer: Arc::clone(&self.writer),
            level: self.level,
            last_timestamp: Arc::clone(&self.last_timestamp),
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
        let (timestamp, diff) = format_timestamp(&self.last_timestamp);
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
                        last_timestamp: Arc::clone(&cached.last_timestamp),
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
                        last_timestamp: Arc::clone(&logger.last_timestamp),
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
        let log_path = prepare_log_path(&log_dir);

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
    let log_path = prepare_log_path(&log_dir);

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

    #[test]
    fn format_timestamp_is_rfc3339_with_offset_and_millis() {
        let last = Mutex::new(0);
        let (timestamp, diff) = format_timestamp(&last);
        assert_eq!(diff, 0, "初回は epoch 全体を差分にしない");

        let parsed = chrono::DateTime::parse_from_rfc3339(&timestamp)
            .expect("timestamp must be RFC3339 with an explicit offset");
        assert_eq!(
            parsed.to_rfc3339_opts(SecondsFormat::Millis, false),
            timestamp
        );
        assert!(timestamp.contains('.'), "ミリ秒が含まれること: {timestamp}");

        let offset = &timestamp[timestamp.len() - 6..];
        assert!(
            offset.starts_with('+') || offset.starts_with('-'),
            "UTC オフセットが明示されること: {timestamp}"
        );
        assert!(offset.contains(':'), "オフセットは HH:MM 形式: {timestamp}");
    }

    #[test]
    fn loggers_have_independent_timestamp_state_and_recover_from_poison() {
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(Box::new(Vec::new())));
        let first = Logger::new(HashMap::new(), Arc::clone(&writer));
        let second = Logger::new(HashMap::new(), Arc::clone(&writer));
        assert!(
            !Arc::ptr_eq(&first.last_timestamp, &second.last_timestamp),
            "ロガー間で LAST_TIMESTAMP を共有しない"
        );

        // 片方のロックをポイズンしても into_inner() で回復し、ログ生成が続く。
        let poisoned = Arc::clone(&first.last_timestamp);
        let _ = std::thread::spawn(move || {
            let _guard = poisoned.lock().unwrap();
            panic!("poison on purpose");
        })
        .join();

        let line = first.build_message(LogLevel::Info, "after poison", None);
        assert!(line.contains("after poison"));
        let other_line = second.build_message(LogLevel::Info, "independent", None);
        assert!(other_line.contains("independent"));
    }

    #[test]
    fn tagged_logger_shares_timestamp_state_with_source_logger() {
        let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(Box::new(Vec::new())));
        let logger = Logger::new(HashMap::new(), Arc::clone(&writer));
        let tagged = logger.tag("component", "test");
        assert!(Arc::ptr_eq(&logger.last_timestamp, &tagged.last_timestamp));
    }

    #[test]
    fn log_file_name_disambiguates_same_second_starts() {
        let base = "2026-11-20T173000";
        let first = log_file_name(base, 111, 1);
        let other_pid = log_file_name(base, 222, 1);
        let other_nanos = log_file_name(base, 111, 2);

        assert_eq!(first, "2026-11-20T173000-111-000000001.log");
        assert_ne!(first, other_pid, "PID が異なれば別ファイル");
        assert_ne!(first, other_nanos, "ナノ秒が異なれば別ファイル");
        assert_ne!(other_pid, other_nanos);
        for name in [&first, &other_pid, &other_nanos] {
            assert!(name.starts_with(base), "既存のタイムスタンプ形式を維持");
            assert!(name.ends_with(".log"));
        }
    }

    #[test]
    fn parse_log_retention_accepts_only_positive_integers() {
        assert_eq!(parse_log_retention("3"), Some(3));
        assert_eq!(parse_log_retention(" 12 "), Some(12));
        assert_eq!(parse_log_retention("0"), None);
        assert_eq!(parse_log_retention("-1"), None);
        assert_eq!(parse_log_retention("abc"), None);
        assert_eq!(parse_log_retention(""), None);
        const { assert!(DEFAULT_LOG_RETENTION > 0) };
        assert!(log_retention() > 0);
    }

    #[test]
    fn prune_old_logs_keeps_only_newest_generations() {
        let dir =
            std::env::temp_dir().join(format!("opencode-util-retention-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        for day in 1..=5 {
            let name = log_file_name(&format!("2026-11-{day:02}T120000"), 42, 0);
            std::fs::write(dir.join(name), b"log line\n").unwrap();
        }
        // ログ形式でないファイルは削除対象外（保持もされる）。
        let unrelated = dir.join("notes.txt");
        std::fs::write(&unrelated, b"keep").unwrap();

        let removed = prune_old_logs(&dir, 2).unwrap();
        assert_eq!(removed, 3, "5 世代 - 保持 2 世代 = 3 件削除");

        let mut remaining: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        remaining.sort();

        assert!(remaining.contains(&"notes.txt".to_string()));
        assert!(remaining.contains(&log_file_name("2026-11-04T120000", 42, 0)));
        assert!(remaining.contains(&log_file_name("2026-11-05T120000", 42, 0)));
        assert!(!remaining.contains(&log_file_name("2026-11-01T120000", 42, 0)));
        assert_eq!(remaining.len(), 3);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn rotated_log_path_keeps_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("opencode-util-rotated-perm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let path = prepare_log_path(&dir);
        let name = path.file_name().and_then(|n| n.to_str()).unwrap();
        let pid_tag = format!("-{}-", std::process::id());
        assert!(name.contains(pid_tag.as_str()));
        assert!(name.ends_with(".log"));

        let file = open_log_file(&path).unwrap();
        drop(file);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "新しいファイル名でも 0600 が維持されること");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

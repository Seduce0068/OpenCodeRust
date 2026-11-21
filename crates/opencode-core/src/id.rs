use rand::RngExt;
use std::sync::Mutex;

const BASE62_CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const LENGTH: usize = 26;

/// 生成する ID の種別を示し、ID 文字列の接頭辞として使われる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefix {
    /// セッション（`ses`）。
    Session,
    /// メッセージ（`msg`）。
    Message,
    /// 権限（`per`）。
    Permission,
    /// 質問（`que`）。
    Question,
    /// ユーザー（`usr`）。
    User,
    /// メッセージ部分（`prt`）。
    Part,
    /// PTY（`pty`）。
    Pty,
    /// ツール（`tool`）。
    Tool,
}

impl Prefix {
    fn as_str(&self) -> &'static str {
        match self {
            Prefix::Session => "ses",
            Prefix::Message => "msg",
            Prefix::Permission => "per",
            Prefix::Question => "que",
            Prefix::User => "usr",
            Prefix::Part => "prt",
            Prefix::Pty => "pty",
            Prefix::Tool => "tool",
        }
    }
}

/// Timestamp and the 12-bit counter share one critical section so a timestamp
/// change and the accompanying counter reset are atomic.
static CLOCK: Mutex<(u64, u32)> = Mutex::new((0, 0));
const COUNTER_MASK: u32 = 0x0FFF;

fn random_base62(length: usize) -> String {
    let mut rng = rand::rng();
    let mut result = String::with_capacity(length);
    for _ in 0..length {
        let idx = rng.random_range(0..62);
        result.push(BASE62_CHARS[idx] as char);
    }
    result
}

fn next_counter(current_timestamp: u64) -> u32 {
    let mut clock = CLOCK.lock().unwrap_or_else(|e| e.into_inner());
    if clock.0 != current_timestamp {
        clock.0 = current_timestamp;
        clock.1 = 0;
    } else {
        clock.1 = (clock.1 + 1) & COUNTER_MASK;
    }
    clock.1
}

/// 種別接頭辞・タイムスタンプ・カウンタ・ランダム文字列から一意な ID を生成する。
///
/// `timestamp` に `None` を渡すと現在時刻（ミリ秒）を使用する。
pub fn create(prefix: Prefix, _descending: bool, timestamp: Option<u64>) -> String {
    let current_timestamp = timestamp.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    });

    let counter_val = next_counter(current_timestamp);
    // A caller-supplied timestamp near `u64::MAX` would overflow the unchecked
    // multiplication/addition. Clamp to the boundary value instead of panicking;
    // the public signature and normal-timestamp output stay unchanged.
    let now = current_timestamp
        .checked_mul(0x1000)
        .and_then(|value| value.checked_add(u64::from(counter_val)))
        .unwrap_or(u64::MAX);
    let time_bytes = now.to_be_bytes();
    let hex_time = hex::encode(time_bytes);
    let random_part = random_base62(LENGTH - 12);

    format!("{}_{}{}", prefix.as_str(), hex_time, random_part)
}

/// ID 文字列に埋め込まれたタイムスタンプ（ミリ秒）を取り出す。形式が不正なら `None`。
pub fn timestamp(id: &str) -> Option<u64> {
    let parts: Vec<&str> = id.split('_').collect();
    if parts.len() != 2 {
        return None;
    }

    let hex = parts[1].get(0..12)?;
    let encoded = u64::from_str_radix(hex, 16).ok()?;
    Some(encoded / 0x1000)
}

/// ID が指定した種別の接頭辞で始まるかを判定する。
pub fn validate_prefix(id: &str, expected: Prefix) -> bool {
    id.starts_with(expected.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_id() {
        let id = create(Prefix::Session, false, None);
        assert!(id.starts_with("ses_"));
        assert_eq!(id.len(), 34);
    }

    #[test]
    fn test_timestamp_extraction() {
        let id = create(Prefix::Session, false, Some(1700000000000));
        let ts = timestamp(&id);
        assert!(ts.is_some());
    }

    #[test]
    fn test_validate_prefix() {
        let id = create(Prefix::Session, false, None);
        assert!(validate_prefix(&id, Prefix::Session));
        assert!(!validate_prefix(&id, Prefix::Message));
    }

    #[test]
    fn test_same_millisecond_ids_are_unique() {
        let ts = 1_700_000_000_000;
        let ids: Vec<String> = (0..100)
            .map(|_| create(Prefix::Message, false, Some(ts)))
            .collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn test_counter_overflow_does_not_corrupt_timestamp() {
        let ts = 1_700_000_000_000;
        let baseline = create(Prefix::Part, false, Some(ts));
        let baseline_ts = timestamp(&baseline);

        // Generate enough IDs in the same millisecond to cross 0x1000. The
        // counter is masked to 12 bits, so the encoded timestamp prefix stays
        // stable instead of spilling into the next millisecond.
        let ids: Vec<String> = (0..0x1010)
            .map(|_| create(Prefix::Part, false, Some(ts)))
            .collect();
        for id in ids {
            assert_eq!(timestamp(&id), baseline_ts);
        }
    }

    #[test]
    fn test_create_id_timestamp_overflow_does_not_panic() {
        // Values whose `timestamp * 0x1000` exceeds u64 must be clamped rather
        // than overflowing in debug builds.
        for ts in [u64::MAX, u64::MAX - 1, (u64::MAX / 0x1000) + 1] {
            let id = create(Prefix::Message, false, Some(ts));
            assert_eq!(id.len(), 34);
            assert!(timestamp(&id).is_some());
        }
    }
}

use crate::identifier::PtyID;
use serde::{Deserialize, Serialize}; // Need to make sure identifier exists

/// PTY（疑似端末）の状態や起動コマンドを表す情報。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtyInfo {
    /// PTY の ID。
    pub id: PtyID,
    /// 表示用のタイトル。
    pub title: String,
    /// 起動したコマンド名。
    pub command: String,
    /// コマンドに渡した引数。
    pub args: Vec<String>,
    /// コマンドの作業ディレクトリ。
    pub cwd: String,
    /// PTY の実行状態。
    pub status: PtyStatus,
    /// 起動したプロセスの PID。
    pub pid: u64,
    /// 終了時の終了コード。実行中は `None`。
    pub exit_code: Option<u64>,
}

/// PTY（疑似端末）の実行状態。
///
/// server 側の PTY reader は EOF ではなく `Err` を検知したときに `Error` を
/// publish し、wire 上は `"error"` として出力される。共有型を信頼する境界で
/// unknown variant として落ちないよう、この共有型にも `Error` を持たせる。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PtyStatus {
    /// プロセスが実行中。
    Running,
    /// プロセスが正常終了済み。
    Exited,
    /// プロセスが読み取りエラー等の異常で終了した。
    Error,
}

/// PTY 新規作成時にクライアントが渡す任意指定の入力。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateInput {
    /// 起動するコマンド名。
    pub command: Option<String>,
    /// コマンドに渡す引数。
    pub args: Option<Vec<String>>,
    /// コマンドの作業ディレクトリ。
    pub cwd: Option<String>,
    /// PTY の表示タイトル。
    pub title: Option<String>,
    /// プロセスに渡す環境変数。
    pub env: Option<std::collections::HashMap<String, String>>,
}

/// PTY の情報更新時に渡す入力。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateInput {
    /// 更新後の表示タイトル。
    pub title: Option<String>,
    /// 更新後の端末サイズ。
    pub size: Option<PtySize>,
}

/// 端末の行数と列数を表すサイズ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtySize {
    /// 端末の行数。
    pub rows: u32,
    /// 端末の列数。
    pub cols: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_pty_info() {
        let info = PtyInfo {
            id: PtyID("pty_123".to_string()),
            title: "Terminal".to_string(),
            command: "bash".to_string(),
            args: vec![],
            cwd: "/".to_string(),
            status: PtyStatus::Running,
            pid: 100,
            exit_code: None,
        };
        let serialized = serde_json::to_string(&info).unwrap();
        let deserialized: PtyInfo = serde_json::from_str(&serialized).unwrap();
        assert_eq!(info, deserialized);
    }

    /// 回帰ガード: server の reader が publish する wire 値 `"error"` が
    /// 共有型 `PtyStatus` へ復元され、再直列化でも落ちないこと。
    #[test]
    fn pty_status_error_wire_value_round_trips() {
        assert_eq!(
            serde_json::to_string(&PtyStatus::Error).unwrap(),
            "\"error\""
        );
        assert_eq!(
            serde_json::from_str::<PtyStatus>("\"error\"").unwrap(),
            PtyStatus::Error
        );
    }

    /// 回帰ガード: `"status":"error"` を含む PtyInfo が共有型で復元できること。
    #[test]
    fn pty_info_with_error_status_round_trips() {
        let wire = r#"{
            "id": "pty_dummy_error",
            "title": "Terminal",
            "command": "dummy-command",
            "args": [],
            "cwd": "/dummy/cwd",
            "status": "error",
            "pid": 100,
            "exit_code": null
        }"#;
        let info: PtyInfo = serde_json::from_str(wire).unwrap();
        assert_eq!(info.status, PtyStatus::Error);
        let serialized = serde_json::to_string(&info).unwrap();
        assert!(serialized.contains("\"status\":\"error\""));
    }
}

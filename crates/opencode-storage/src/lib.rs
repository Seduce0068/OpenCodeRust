//! SQLite によるデータ永続化を担うクレート。
//!
//! `schema` が sessions / messages / parts / todos / session_input などの
//! テーブル定義とマイグレーションを保持し、`database` が接続とマイグレーション
//! 実行を提供する。`repository` はセッション・メッセージ・Todo・入力キューへの
//! CRUD 操作をまとめる。
//!
//! # 使い方
//!
//! `Storage::new(db_path)` で接続し、`migrate()` でスキーマを適用した上で
//! `SessionRepository` などのリポジトリを利用する。

pub mod database;
pub mod repository;
pub(crate) mod schema;

pub use database::*;
pub use repository::{
    InputRepository, MessageRepository, SessionRepository, TodoItem, TodoRepository,
};

use anyhow::Result;
use sqlx::{Pool, Sqlite};

pub struct Storage {
    pool: Pool<Sqlite>,
}

impl Storage {
    pub async fn new(db_path: &str) -> Result<Self> {
        // Database と同じ pool options を使い、全接続で PRAGMA foreign_keys = ON を適用する。
        // 直接 SqlitePool::connect すると FK が OFF のプールになり ON DELETE CASCADE が
        // 発動しないため、必ず database::sqlite_pool_options 経由で生成する。
        let pool = crate::database::sqlite_pool_options(5)
            .connect(&format!("sqlite:{}", db_path))
            .await?;
        Ok(Self { pool })
    }

    /// 正規スキーマ（`schema::ALL_MIGRATIONS`）を単一トランザクションで適用する。
    ///
    /// 以前は最小限の `sessions` テーブル（`id/title/time_created/time_updated`）
    /// だけを作る別経路だったため、`Database` 経由の DB とスキーマが食い違い、
    /// この DB に対して `SessionRepository` を使うと NOT NULL 列不足で失敗した。
    /// 現在は `database::migrate_pool` に委譲し、`Database::run_migrations` と
    /// マイグレーション経路を完全に統一している（user_version の確認・記録含む）。
    pub async fn migrate(&self) -> Result<()> {
        crate::database::migrate_pool(&self.pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Storage;
    use sqlx::Row;
    use std::path::PathBuf;

    fn temp_db_path() -> PathBuf {
        let name = format!("storage-migrate-{}.db", uuid::Uuid::new_v4());
        std::env::temp_dir().join(name)
    }

    async fn column_names(pool: &sqlx::SqlitePool, table: &str) -> Vec<String> {
        let sql = format!("PRAGMA table_info({table});");
        let rows = sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .fetch_all(pool)
            .await
            .expect("table_info");
        rows.iter()
            .map(|row| row.try_get::<String, _>("name").expect("column name"))
            .collect()
    }

    #[tokio::test]
    async fn migrate_creates_canonical_schema_and_is_idempotent() {
        let path = temp_db_path();
        // Storage::new は mode=rwc を付けないため、事前ファイル作成が必要。
        std::fs::write(&path, b"").expect("create temp db file");
        let path_str = path.to_string_lossy().into_owned();
        let storage = Storage::new(&path_str)
            .await
            .expect("storage connects to temp db");

        storage.migrate().await.expect("first migration succeeds");
        storage.migrate().await.expect("second migration succeeds");

        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{path_str}"))
            .await
            .expect("reopen temp db");

        // 旧的最小スキーマのシンボルは消え、正規スキーマの列が存在する。
        let columns = column_names(&pool, "sessions").await;
        assert!(columns.contains(&"project_id".to_string()));
        assert!(columns.contains(&"slug".to_string()));
        assert!(columns.contains(&"created_at".to_string()));
        assert!(columns.contains(&"updated_at".to_string()));
        assert!(!columns.contains(&"time_created".to_string()));
        assert!(!columns.contains(&"time_updated".to_string()));

        // v3 の messages JSON 列もマイグレーションで作られている。
        let message_columns = column_names(&pool, "messages").await;
        assert!(
            message_columns.contains(&"metadata".to_string()),
            "messages.metadata must exist after migration"
        );
        assert!(
            message_columns.contains(&"usage".to_string()),
            "messages.usage must exist after migration"
        );

        // ALL_MIGRATIONS の他のテーブルも作られている。
        for table in ["messages", "parts", "todos", "session_input"] {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
            )
            .bind(table)
            .fetch_one(&pool)
            .await
            .expect("sqlite_master readable");
            assert_eq!(count, 1, "{table} table must exist");
        }

        drop(pool);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn migrate_records_user_version_like_database_path() {
        let path = temp_db_path();
        // Storage::new は mode=rwc を付けないため、事前ファイル作成が必要。
        std::fs::write(&path, b"").expect("create temp db file");
        let path_str = path.to_string_lossy().into_owned();
        let storage = Storage::new(&path_str)
            .await
            .expect("storage connects to temp db");

        storage.migrate().await.expect("migration succeeds");

        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{path_str}"))
            .await
            .expect("reopen temp db");

        // Database 経路と同じ SCHEMA_VERSION が Storage 経路でも記録される。
        // これは両経路が database::migrate_pool を共有している直接的な証拠。
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await
            .expect("user_version readable");
        assert_eq!(version, i64::from(crate::database::SCHEMA_VERSION));

        // Database 経路のマイグレーションが補う metadata 列も同じく作られる。
        let columns = column_names(&pool, "session_input").await;
        assert!(
            columns.contains(&"metadata".to_string()),
            "session_input.metadata must exist after the unified migration"
        );

        drop(pool);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn migrate_skips_when_user_version_is_current() {
        let path = temp_db_path();
        std::fs::write(&path, b"").expect("create temp db file");
        let path_str = path.to_string_lossy().into_owned();
        let storage = Storage::new(&path_str)
            .await
            .expect("storage connects to temp db");

        storage.migrate().await.expect("first migration succeeds");

        // 適用済みバージョンを進め、再実行で書き戻されないことを確認する。
        // Database 経路と同じスキップ判定が効いている必要がある。
        let advanced = i64::from(crate::database::SCHEMA_VERSION) + 1;
        {
            let mut conn = storage.pool.acquire().await.expect("acquire");
            let sql = format!("PRAGMA user_version = {advanced};");
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                .execute(&mut *conn)
                .await
                .expect("advance user_version");
        }

        storage.migrate().await.expect("second migration succeeds");

        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{path_str}"))
            .await
            .expect("reopen temp db");
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&pool)
            .await
            .expect("user_version readable");
        assert_eq!(
            version, advanced,
            "second run must not rewrite user_version"
        );

        drop(pool);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn storage_pool_enables_foreign_keys() {
        let path = temp_db_path();
        std::fs::write(&path, b"").expect("create temp db file");
        let path_str = path.to_string_lossy().into_owned();
        let storage = Storage::new(&path_str)
            .await
            .expect("storage connects to temp db");

        // Storage::new は database::sqlite_pool_options 経由でプールを生成するため、
        // Database 経路と同じく接続ごとに foreign_keys が ON になる。
        let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&storage.pool)
            .await
            .expect("foreign_keys pragma readable");
        assert_eq!(enabled, 1, "Storage pool must have foreign_keys ON");

        drop(storage);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn storage_session_delete_cascades_to_children() {
        let path = temp_db_path();
        std::fs::write(&path, b"").expect("create temp db file");
        let path_str = path.to_string_lossy().into_owned();
        let storage = Storage::new(&path_str)
            .await
            .expect("storage connects to temp db");
        storage.migrate().await.expect("migration succeeds");

        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) VALUES ('storage-fk-session', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(&storage.pool)
        .await
        .expect("session row exists");
        sqlx::query(
            "INSERT INTO messages (id, session_id, role, created_at) VALUES ('storage-fk-message', 'storage-fk-session', 'user', 1)",
        )
        .execute(&storage.pool)
        .await
        .expect("message row exists");

        // SessionRepository::delete と同じ DELETE を Storage プール上で実行する。
        sqlx::query("DELETE FROM sessions WHERE id = 'storage-fk-session'")
            .execute(&storage.pool)
            .await
            .expect("delete session");

        // FK が OFF だと子行が残るため、cascade の発動を直接検証する。
        let remaining: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE session_id = ?")
                .bind("storage-fk-session")
                .fetch_one(&storage.pool)
                .await
                .expect("count messages");
        assert_eq!(remaining, 0, "message must cascade-delete via Storage pool");

        drop(storage);
        let _ = std::fs::remove_file(&path);
    }
}

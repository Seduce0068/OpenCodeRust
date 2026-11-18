pub mod database;
pub mod repository;
pub mod schema;

pub use database::*;
pub use repository::{
    InputRepository, MessageRepository, SessionRepository, TodoItem, TodoRepository,
};
pub use schema::*;

use anyhow::Result;
use sqlx::{sqlite::SqlitePool, Pool, Sqlite};

pub struct Storage {
    pool: Pool<Sqlite>,
}

impl Storage {
    pub async fn new(db_path: &str) -> Result<Self> {
        let pool = SqlitePool::connect(&format!("sqlite:{}", db_path)).await?;
        Ok(Self { pool })
    }

    /// 正規スキーマ（`schema::ALL_MIGRATIONS`）を単一トランザクションで適用する。
    ///
    /// 以前は最小限の `sessions` テーブル（`id/title/time_created/time_updated`）
    /// だけを作る別経路だったため、`Database` 経由の DB とスキーマが食い違い、
    /// この DB に対して `SessionRepository` を使うと NOT NULL 列不足で失敗した。
    /// スキーマ定義の単一ソースは `schema.rs` に統一する。
    pub async fn migrate(&self) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for migration in crate::schema::ALL_MIGRATIONS {
            sqlx::raw_sql(*migration).execute(&mut *tx).await?;
        }
        tx.commit().await?;
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
}

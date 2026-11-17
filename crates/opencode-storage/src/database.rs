use anyhow::Result;
use sqlx::sqlite::{SqliteConnection, SqlitePool, SqlitePoolOptions};
use sqlx::{Row, Sqlite, Transaction};
use std::future::Future;
use std::path::PathBuf;
use thiserror::Error;
use tracing::info;

/// 現在のスキーマバージョン。`PRAGMA user_version` に記録し、適用済みの
/// マイグレーションを再実行しないための比較値に使う。
///
/// 既存 DB は `user_version = 0`（SQLite のデフォルト）なので、
/// 初回起動時は必ずマイグレーションが実行される。
const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum DatabaseError {
    #[error("Database connection error: {0}")]
    ConnectionError(String),

    #[error("Migration error: {0}")]
    MigrationError(String),

    #[error("Query error: {0}")]
    QueryError(String),

    #[error("Transaction error: {0}")]
    TransactionError(String),
}

pub struct Database {
    pool: SqlitePool,
}

pub type SqliteTransaction<'a> = Transaction<'a, Sqlite>;

impl Database {
    pub async fn new() -> Result<Self, DatabaseError> {
        let db_path = Self::get_database_path()?;

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DatabaseError::ConnectionError(e.to_string()))?;
        }

        let db_url = format!("sqlite:{}?mode=rwc", db_path.display());

        info!("Connecting to database at {}", db_path.display());

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&db_url)
            .await
            .map_err(|e| DatabaseError::ConnectionError(e.to_string()))?;

        let db = Self { pool };
        db.run_migrations().await?;

        Ok(db)
    }

    pub async fn in_memory() -> Result<Self, DatabaseError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .map_err(|e| DatabaseError::ConnectionError(e.to_string()))?;

        let db = Self { pool };
        db.run_migrations().await?;

        Ok(db)
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn begin(&self) -> Result<SqliteTransaction<'_>, DatabaseError> {
        self.pool
            .begin()
            .await
            .map_err(|e| DatabaseError::TransactionError(e.to_string()))
    }

    pub async fn transaction<F, T, Fut>(&self, f: F) -> Result<T, DatabaseError>
    where
        F: FnOnce(&mut SqliteTransaction<'_>) -> Fut,
        Fut: Future<Output = Result<T, DatabaseError>>,
    {
        let mut tx = self.begin().await?;
        let result = f(&mut tx).await?;
        tx.commit()
            .await
            .map_err(|e| DatabaseError::TransactionError(e.to_string()))?;
        Ok(result)
    }

    pub async fn get_connection(&self) -> Result<SqliteConnection, DatabaseError> {
        self.pool
            .acquire()
            .await
            .map(|conn| conn.detach())
            .map_err(|e| DatabaseError::ConnectionError(e.to_string()))
    }

    async fn run_migrations(&self) -> Result<(), DatabaseError> {
        info!("Running database migrations");

        // 全スキーマ文を単一トランザクションで実行する。個別に auto-commit すると
        // 途中失敗時に一部のテーブル/インデックスだけが残り、再起動時に
        // 不整合なスキーマへ migration が走る恐れがある。
        let mut tx = self.begin().await?;

        // 適用済みバージョンを確認する。user_version=0 の既存 DB は
        // 初回移行の対象として扱い、SCHEMA_VERSION 以上ならスキップする。
        let row = sqlx::query("PRAGMA user_version")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| DatabaseError::MigrationError(e.to_string()))?;
        let current_version = row
            .try_get::<i64, _>("user_version")
            .map_err(|e| DatabaseError::MigrationError(e.to_string()))?;

        if current_version >= i64::from(SCHEMA_VERSION) {
            info!(
                "Schema already at version {} (user_version={}), skipping migrations",
                SCHEMA_VERSION, current_version
            );
            tx.commit()
                .await
                .map_err(|e| DatabaseError::MigrationError(e.to_string()))?;
            return Ok(());
        }

        for migration in crate::schema::ALL_MIGRATIONS {
            sqlx::raw_sql(*migration)
                .execute(&mut *tx)
                .await
                .map_err(|e| DatabaseError::MigrationError(e.to_string()))?;
        }

        // metadata 列は CREATE_SESSION_INPUT_TABLE_WITH_METADATA に含まれている。
        // 旧スキーマの DB だけに ALTER を適用する。SQLite のエラー文字列
        // ("duplicate column name") を照合するのは版依存で脆いため、
        // PRAGMA table_info で列の有無を構造的に判定する。
        let input_columns = sqlx::query("PRAGMA table_info(session_input)")
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| DatabaseError::MigrationError(e.to_string()))?;
        let has_metadata = input_columns.iter().any(|row| {
            row.try_get::<String, _>("name")
                .map(|name| name == "metadata")
                .unwrap_or(false)
        });

        if !has_metadata {
            sqlx::raw_sql(crate::schema::MIGRATE_SESSION_INPUT_METADATA)
                .execute(&mut *tx)
                .await
                .map_err(|e| DatabaseError::MigrationError(e.to_string()))?;
        }

        // バージョン記録も同じトランザクションに含め、DDL と原子的に確定させる。
        // 埋め込む値はコード内定数だけ（外部入力なし）なので監査済みとして扱う。
        let set_version = format!("PRAGMA user_version = {SCHEMA_VERSION};");
        sqlx::raw_sql(sqlx::AssertSqlSafe(set_version))
            .execute(&mut *tx)
            .await
            .map_err(|e| DatabaseError::MigrationError(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| DatabaseError::MigrationError(e.to_string()))?;

        Ok(())
    }

    fn get_database_path() -> Result<PathBuf, DatabaseError> {
        let data_dir = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("opencode");

        Ok(data_dir.join("opencode.db"))
    }
}

#[cfg(test)]
mod migration_tests {
    use super::{Database, SCHEMA_VERSION};
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::SqlitePool;

    async fn read_user_version(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(pool)
            .await
            .expect("read user_version")
    }

    async fn set_user_version(pool: &SqlitePool, version: i64) {
        // 引数はテスト内固定値のみ（外部入力なし）。
        let sql = format!("PRAGMA user_version = {version};");
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(pool)
            .await
            .expect("set user_version");
    }

    #[tokio::test]
    async fn first_migration_records_schema_version() {
        let database = Database::in_memory().await.expect("database initializes");
        assert_eq!(
            read_user_version(database.pool()).await,
            i64::from(SCHEMA_VERSION)
        );
    }

    #[tokio::test]
    async fn zero_version_database_is_migrated() {
        // 既存 DB と同じ user_version=0 の状態から移行できることを確認する。
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("empty database");
        assert_eq!(
            read_user_version(&pool).await,
            0,
            "fresh DB starts at version 0"
        );

        let database = Database { pool };
        database.run_migrations().await.expect("migration succeeds");

        assert_eq!(
            read_user_version(database.pool()).await,
            i64::from(SCHEMA_VERSION)
        );
        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
        )
        .fetch_one(database.pool())
        .await
        .expect("sqlite_master readable");
        assert_eq!(table_count, 1, "sessions table created on first migration");
    }

    #[tokio::test]
    async fn second_migration_run_is_skipped() {
        let database = Database::in_memory().await.expect("database initializes");
        assert_eq!(
            read_user_version(database.pool()).await,
            i64::from(SCHEMA_VERSION)
        );

        // 適用済みバージョンが SCHEMA_VERSION 以上なら再実行しない。
        // 再実行されれば user_version は SCHEMA_VERSION に書き戻されるため、
        // この値が保持されることはスキップの直接的な証拠になる。
        let advanced = i64::from(SCHEMA_VERSION) + 1;
        set_user_version(database.pool(), advanced).await;
        database
            .run_migrations()
            .await
            .expect("second run succeeds");
        assert_eq!(
            read_user_version(database.pool()).await,
            advanced,
            "second run must not rewrite user_version"
        );
    }
}

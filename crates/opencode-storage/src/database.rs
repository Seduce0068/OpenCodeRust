use anyhow::Result;
use sqlx::sqlite::{SqliteConnection, SqlitePool, SqlitePoolOptions};
use sqlx::{Row, Sqlite, Transaction};
use std::future::Future;
use std::path::PathBuf;
use thiserror::Error;
use tracing::info;

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

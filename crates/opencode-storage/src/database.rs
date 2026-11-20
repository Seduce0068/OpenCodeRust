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
/// v2: sessions.metadata 列を追加（session_input.metadata は既存）。
/// v3: messages.metadata / messages.usage 列を追加（メッセージ単位の metadata/usage 永続化）。
pub(crate) const SCHEMA_VERSION: u32 = 3;

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

    /// 保存済みデータが壊れていて安全に読み取れない場合。
    ///
    /// 黙って行を捨てると、その結果を `replace_for_session` に渡した際に
    /// 元行が物理削除されるため、読み取り側・書き込み側の両方で明示的に失敗させる。
    #[error("Corrupt stored data: {0}")]
    CorruptData(String),
}

pub struct Database {
    pool: SqlitePool,
}

pub type SqliteTransaction<'a> = Transaction<'a, Sqlite>;

/// 正規スキーマ（`schema::ALL_MIGRATIONS`）を `pool` に対して単一トランザクションで
/// 適用する。適用済みバージョン（`PRAGMA user_version`）の確認と記録まで含め、
/// `Database::run_migrations` と `Storage::migrate` の両経路がこの関数を共有する。
///
/// `SCHEMA_VERSION` はこの関数が唯一書き込むため、両経路で同じ定数が使われる。
pub(crate) async fn migrate_pool(pool: &SqlitePool) -> Result<(), DatabaseError> {
    info!("Running database migrations");

    // 全スキーマ文を単一トランザクションで実行する。個別に auto-commit すると
    // 途中失敗時に一部のテーブル/インデックスだけが残り、再起動時に
    // 不整合なスキーマへ migration が走る恐れがある。
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| DatabaseError::TransactionError(e.to_string()))?;

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

    // 旧バージョンの DB では ADD COLUMN 済みの列が既に存在する。
    // 列ごとに PRAGMA で存在を判定し、二重追加になる ALTER だけスキップする
    // （CREATE TABLE / CREATE INDEX は IF NOT EXISTS なので再実行しても安全）。
    let session_columns = sqlx::query("PRAGMA table_info(sessions)")
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| DatabaseError::MigrationError(e.to_string()))?;
    let message_columns = sqlx::query("PRAGMA table_info(messages)")
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| DatabaseError::MigrationError(e.to_string()))?;

    for migration in crate::schema::ALL_MIGRATIONS {
        let already_applied = if *migration == crate::schema::MIGRATE_SESSION_METADATA {
            has_column(&session_columns, "metadata")
        } else if *migration == crate::schema::MIGRATE_MESSAGES_METADATA {
            has_column(&message_columns, "metadata")
        } else if *migration == crate::schema::MIGRATE_MESSAGES_USAGE {
            has_column(&message_columns, "usage")
        } else {
            false
        };
        if already_applied {
            continue;
        }
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

/// `PRAGMA table_info` の結果に `name` 列が存在するかを構造的に判定する。
/// SQLite のエラー文字列（"duplicate column name"）に依存しないため、
/// バージョンをまたいだ再実行でも安全に列追加をスキップできる。
fn has_column(rows: &[sqlx::sqlite::SqliteRow], name: &str) -> bool {
    rows.iter().any(|row| {
        row.try_get::<String, _>("name")
            .map(|column| column == name)
            .unwrap_or(false)
    })
}

/// すべてのプール生成経路（`Database::new` / `Database::in_memory` / `Storage::new`）が
/// 共有する接続設定。
///
/// SQLite は `PRAGMA foreign_keys` が既定で OFF のため、`schema.rs` の
/// `ON DELETE CASCADE`（6 件）が何も発動しない。`after_connect` はプールが
/// 新規接続を確立するたびに呼ばれるので、ここで有効化すれば
/// 通常クエリにも `begin()` 由来のトランザクションにも同じ設定が適用される。
pub(crate) fn sqlite_pool_options(max_connections: u32) -> SqlitePoolOptions {
    SqlitePoolOptions::new()
        .max_connections(max_connections)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query("PRAGMA foreign_keys = ON;")
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
}

impl Database {
    /// `Database` が実際に接続する SQLite ファイルのパスを返す。
    ///
    /// `OPENCODE_DATA_DIR` に空でない値が設定されていれば、その値（前後の空白は
    /// 除去）を `dirs::data_local_dir()` の代わりのデータディレクトリ基数として
    /// 優先する。空文字・空白のみ・非 UTF-8 の値は不正として無視し、従来どおり
    /// `dirs::data_local_dir()`（取得不能なら `.`）を使う。
    ///
    /// 解決したパスはログやエラーには含めない。CLI の `db path` も同じ helper を
    /// 参照し、バックアップ対象と表示値がずれないようにする。
    pub fn database_path() -> PathBuf {
        let base = match std::env::var("OPENCODE_DATA_DIR") {
            // 前後空白を除いた値が残る場合だけ上書きとして採用する。
            // 空・空白のみ・非 UTF-8（Err）は従来の既定へフォールバック。
            Ok(value) if !value.trim().is_empty() => PathBuf::from(value.trim()),
            _ => dirs::data_local_dir().unwrap_or_else(|| PathBuf::from(".")),
        };

        base.join("opencode").join("opencode.db")
    }

    pub async fn new() -> Result<Self, DatabaseError> {
        let db_path = Self::get_database_path()?;

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| DatabaseError::ConnectionError(e.to_string()))?;
        }

        let db_url = format!("sqlite:{}?mode=rwc", db_path.display());

        // 解決済みパスはログに出さない（OPENCODE_DATA_DIR の値を漏らさない）。
        info!("Connecting to database");

        let pool = sqlite_pool_options(5)
            .connect(&db_url)
            .await
            .map_err(|e| DatabaseError::ConnectionError(e.to_string()))?;

        let db = Self { pool };
        db.run_migrations().await?;

        Ok(db)
    }

    pub async fn in_memory() -> Result<Self, DatabaseError> {
        let pool = sqlite_pool_options(1)
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
        // 単一の実装に委譲することで、Database 経路と Storage 経路の
        // マイグレーション動作（user_version の確認・記録を含む）を完全に同一にする。
        migrate_pool(&self.pool).await
    }

    /// パス解決は公開 helper に一本化し、ここは互換のための委譲だけを行う。
    fn get_database_path() -> Result<PathBuf, DatabaseError> {
        Ok(Self::database_path())
    }
}

#[cfg(test)]
mod migration_tests {
    use super::{sqlite_pool_options, Database, SCHEMA_VERSION};
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::{Row, SqlitePool};

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

    async fn count_sessions(pool: &SqlitePool, id: &str) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .expect("count sessions")
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

    /// v2 DB（sessions.metadata 追加済み・messages に新列なし・user_version=2）から
    /// v3 へ移行できることを固定する。v2 の ALTER を再実行すると
    /// "duplicate column name: metadata" で失敗するため、列存在チェックが必須。
    #[tokio::test]
    async fn v2_database_migrates_to_v3_without_duplicate_column() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("empty database");

        // v2 時点のスキーマを手組みで再現する（v3 の 2 本の ALTER は含めない）。
        for migration in [
            crate::schema::CREATE_SESSIONS_TABLE_LEGACY,
            crate::schema::MIGRATE_SESSION_METADATA,
            crate::schema::CREATE_MESSAGES_TABLE,
            crate::schema::CREATE_PARTS_TABLE,
            crate::schema::CREATE_TODOS_TABLE,
            crate::schema::CREATE_PERMISSIONS_TABLE,
            crate::schema::CREATE_SESSION_SHARES_TABLE,
            crate::schema::CREATE_SESSION_INPUT_TABLE_WITH_METADATA,
            crate::schema::CREATE_INDEXES,
            crate::schema::IDX_SESSION_INPUT,
        ] {
            sqlx::raw_sql(migration)
                .execute(&pool)
                .await
                .expect("v2 schema statement");
        }

        // 既存データ（セッション metadata とメッセージ）が移行で消えないこと。
        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at, metadata) \
             VALUES ('v2-session', 'project-1', 'test', '.', 'Test', 1, 1, '{\"kept\":true}')",
        )
        .execute(&pool)
        .await
        .expect("v2 session row");
        sqlx::query(
            "INSERT INTO messages (id, session_id, role, created_at, data) \
             VALUES ('v2-message', 'v2-session', 'user', 1, '[]')",
        )
        .execute(&pool)
        .await
        .expect("v2 message row");
        set_user_version(&pool, 2).await;

        let database = Database { pool };
        database
            .run_migrations()
            .await
            .expect("v2 -> v3 migration must not fail on already-existing columns");

        assert_eq!(
            read_user_version(database.pool()).await,
            i64::from(SCHEMA_VERSION),
            "v3 version must be recorded"
        );
        let rows = sqlx::query("PRAGMA table_info(messages)")
            .fetch_all(database.pool())
            .await
            .expect("table_info readable");
        let names: Vec<String> = rows
            .iter()
            .map(|row| row.try_get::<String, _>("name").expect("column name"))
            .collect();
        assert!(names.iter().any(|name| name == "metadata"));
        assert!(names.iter().any(|name| name == "usage"));

        let metadata: String = sqlx::query_scalar("SELECT metadata FROM sessions WHERE id = ?")
            .bind("v2-session")
            .fetch_one(database.pool())
            .await
            .expect("session metadata readable");
        assert_eq!(
            metadata, "{\"kept\":true}",
            "既存 metadata は変更されないこと"
        );
        assert_eq!(
            count_sessions(database.pool(), "v2-session").await,
            1,
            "既存セッションが残ること"
        );
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

    #[tokio::test]
    async fn migration_failure_rolls_back_partial_schema() {
        // sessions を project_id なしで作り、CREATE INDEX を確実に失敗させる。
        // これによりマイグレーションの途中（先行する CREATE TABLE の後）で
        // エラーが起き、トランザクション全体がロールバックされることを検証する。
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("empty database");
        sqlx::query("CREATE TABLE sessions (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .expect("pre-existing sessions table");

        let database = Database { pool };
        assert!(
            database.run_migrations().await.is_err(),
            "migration must fail on the conflicting schema"
        );

        assert_eq!(
            read_user_version(database.pool()).await,
            0,
            "failed migration must not record user_version"
        );
        let messages: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'messages'",
        )
        .fetch_one(database.pool())
        .await
        .expect("sqlite_master readable");
        assert_eq!(
            messages, 0,
            "tables created before the failure must be rolled back"
        );
    }

    #[tokio::test]
    async fn begin_transaction_commits_and_rolls_back() {
        let database = Database::in_memory().await.expect("database initializes");

        // commit した行は永続化される（run_migrations と同じトランザクション経路）。
        {
            let mut tx = database.begin().await.expect("begin");
            sqlx::query(
                "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) VALUES ('tx-commit', 'project-1', 'test', '.', 'Test', 1, 1)",
            )
            .execute(&mut *tx)
            .await
            .expect("insert inside committed transaction");
            tx.commit().await.expect("commit");
        }
        assert_eq!(
            count_sessions(database.pool(), "tx-commit").await,
            1,
            "committed row must be visible"
        );

        // commit せずにドロップしたトランザクションはロールバックされる。
        {
            let mut tx = database.begin().await.expect("begin");
            sqlx::query(
                "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) VALUES ('tx-rollback', 'project-1', 'test', '.', 'Test', 1, 1)",
            )
            .execute(&mut *tx)
            .await
            .expect("insert inside rolled-back transaction");
            // tx を commit せずにドロップ → 暗黙のロールバック
        }
        assert_eq!(
            count_sessions(database.pool(), "tx-rollback").await,
            0,
            "uncommitted row must be rolled back"
        );
    }

    #[tokio::test]
    async fn in_memory_databases_are_isolated() {
        // in_memory() は接続ごとに private な SQLite DB を持つ（max_connections=1）。
        // 別インスタンス間でデータが漏れないことを確認する。
        let first = Database::in_memory().await.expect("first database");
        let second = Database::in_memory().await.expect("second database");

        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) VALUES ('isolated', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(first.pool())
        .await
        .expect("insert into first database");
        assert_eq!(count_sessions(first.pool(), "isolated").await, 1);
        assert_eq!(
            count_sessions(second.pool(), "isolated").await,
            0,
            "each in_memory database must have its own storage"
        );
    }

    #[tokio::test]
    async fn foreign_keys_are_enabled_on_every_pool_creation_path() {
        // in_memory() 経由（max_connections=1）。
        let database = Database::in_memory().await.expect("database initializes");
        let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(database.pool())
            .await
            .expect("foreign_keys pragma readable");
        assert_eq!(enabled, 1, "in_memory pool must have foreign_keys ON");

        // Database::new() と同じ helper で作ったファイル DB 経由（max_connections=5 と同種）。
        let path = std::env::temp_dir().join(format!("storage-fk-{}.db", uuid::Uuid::new_v4()));
        let url = format!("sqlite:{}?mode=rwc", path.display());
        let pool = sqlite_pool_options(2)
            .connect(&url)
            .await
            .expect("file-backed pool connects");
        let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&pool)
            .await
            .expect("foreign_keys pragma readable");
        assert_eq!(enabled, 1, "file-backed pool must have foreign_keys ON");

        drop(pool);
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn deleting_session_cascades_to_all_child_tables() {
        // 実 DB（in_memory の SQLite）に対して 6 つの FK のうち session_id を
        // 参照する 5 件がすべて発動することを確認する。
        let database = Database::in_memory().await.expect("database initializes");
        let pool = database.pool();

        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) \
             VALUES ('fk-session', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(pool)
        .await
        .expect("session row exists");
        sqlx::query(
            "INSERT INTO messages (id, session_id, role, created_at) \
             VALUES ('fk-message', 'fk-session', 'user', 1)",
        )
        .execute(pool)
        .await
        .expect("message row exists");
        sqlx::query(
            "INSERT INTO parts (id, message_id, session_id, created_at, part_type) \
             VALUES ('fk-part', 'fk-message', 'fk-session', 1, 'text')",
        )
        .execute(pool)
        .await
        .expect("part row exists");
        sqlx::query(
            "INSERT INTO todos (session_id, todo_id, content, position, created_at, updated_at) \
             VALUES ('fk-session', 'fk-todo', 'todo', 0, 1, 1)",
        )
        .execute(pool)
        .await
        .expect("todo row exists");
        sqlx::query(
            "INSERT INTO session_shares (session_id, id, secret, url, created_at) \
             VALUES ('fk-session', 'fk-share', 'secret', 'https://example.com', 1)",
        )
        .execute(pool)
        .await
        .expect("share row exists");
        sqlx::query(
            "INSERT INTO session_input (id, session_id, prompt, status, created_at) \
             VALUES ('fk-input', 'fk-session', 'prompt', 'pending', 1)",
        )
        .execute(pool)
        .await
        .expect("input row exists");

        sqlx::query("DELETE FROM sessions WHERE id = 'fk-session'")
            .execute(pool)
            .await
            .expect("delete session");

        // Literal SQL per table: sqlx 0.9 rejects dynamic query strings.
        let counts = [
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM messages WHERE session_id = ?")
                .bind("fk-session")
                .fetch_one(pool)
                .await
                .expect("count messages"),
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM parts WHERE session_id = ?")
                .bind("fk-session")
                .fetch_one(pool)
                .await
                .expect("count parts"),
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM todos WHERE session_id = ?")
                .bind("fk-session")
                .fetch_one(pool)
                .await
                .expect("count todos"),
            sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM session_shares WHERE session_id = ?",
            )
            .bind("fk-session")
            .fetch_one(pool)
            .await
            .expect("count session_shares"),
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM session_input WHERE session_id = ?")
                .bind("fk-session")
                .fetch_one(pool)
                .await
                .expect("count session_input"),
        ];
        for (table, count) in [
            ("messages", counts[0]),
            ("parts", counts[1]),
            ("todos", counts[2]),
            ("session_shares", counts[3]),
            ("session_input", counts[4]),
        ] {
            assert_eq!(
                count, 0,
                "{table} rows must cascade-delete with the session"
            );
        }
    }

    #[tokio::test]
    async fn deleting_message_cascades_to_its_parts() {
        // 残る 1 件の FK（parts.message_id）も個別に検証する。
        let database = Database::in_memory().await.expect("database initializes");
        let pool = database.pool();

        sqlx::query(
            "INSERT INTO sessions (id, project_id, slug, directory, title, created_at, updated_at) \
             VALUES ('fk-parent', 'project-1', 'test', '.', 'Test', 1, 1)",
        )
        .execute(pool)
        .await
        .expect("session row exists");
        sqlx::query(
            "INSERT INTO messages (id, session_id, role, created_at) \
             VALUES ('fk-msg-2', 'fk-parent', 'assistant', 1)",
        )
        .execute(pool)
        .await
        .expect("message row exists");
        sqlx::query(
            "INSERT INTO parts (id, message_id, session_id, created_at, part_type) \
             VALUES ('fk-part-2', 'fk-msg-2', 'fk-parent', 1, 'text')",
        )
        .execute(pool)
        .await
        .expect("part row exists");

        sqlx::query("DELETE FROM messages WHERE id = 'fk-msg-2'")
            .execute(pool)
            .await
            .expect("delete message");

        let parts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM parts WHERE message_id = ?")
            .bind("fk-msg-2")
            .fetch_one(pool)
            .await
            .expect("count parts");
        assert_eq!(parts, 0, "parts must cascade-delete with their message");
        assert_eq!(
            count_sessions(pool, "fk-parent").await,
            1,
            "deleting a message must not delete its parent session"
        );
    }
}

#[cfg(test)]
mod path_tests {
    use super::Database;
    use std::path::PathBuf;
    use std::sync::Mutex;

    /// `OPENCODE_DATA_DIR` を書き換えるテストを直列化する。
    static DATA_DIR_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct RestoreDataDir(Option<std::ffi::OsString>);

    impl Drop for RestoreDataDir {
        fn drop(&mut self) {
            match &self.0 {
                Some(value) => std::env::set_var("OPENCODE_DATA_DIR", value),
                None => std::env::remove_var("OPENCODE_DATA_DIR"),
            }
        }
    }

    fn default_path() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("opencode")
            .join("opencode.db")
    }

    #[test]
    fn database_path_prefers_trimmed_data_dir_override() {
        let _guard = DATA_DIR_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = RestoreDataDir(std::env::var_os("OPENCODE_DATA_DIR"));

        let base = std::env::temp_dir().join(format!("opencode-db-path-{}", uuid::Uuid::new_v4()));
        std::env::set_var("OPENCODE_DATA_DIR", format!("  {}  ", base.display()));

        assert_eq!(
            Database::database_path(),
            base.join("opencode").join("opencode.db"),
            "OPENCODE_DATA_DIR must replace the data_local_dir base"
        );
    }

    #[test]
    fn database_path_ignores_empty_or_invalid_values() {
        let _guard = DATA_DIR_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _restore = RestoreDataDir(std::env::var_os("OPENCODE_DATA_DIR"));

        for value in ["", "   ", "\t\n"] {
            std::env::set_var("OPENCODE_DATA_DIR", value);
            assert_eq!(
                Database::database_path(),
                default_path(),
                "invalid OPENCODE_DATA_DIR={value:?} must fall back"
            );
        }

        std::env::remove_var("OPENCODE_DATA_DIR");
        assert_eq!(
            Database::database_path(),
            default_path(),
            "unset OPENCODE_DATA_DIR must use the conventional base"
        );
    }
}

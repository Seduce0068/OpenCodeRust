// ============================================================================
// SQLite Schema Definitions
// Based on TypeScript: /opencode/packages/opencode/src/session/session.sql.ts
// ============================================================================

/// Sessions table - stores session metadata
pub const CREATE_SESSIONS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    parent_id TEXT,
    slug TEXT NOT NULL,
    directory TEXT NOT NULL,
    title TEXT NOT NULL,
    version TEXT NOT NULL DEFAULT '1.0.0',
    share_url TEXT,

    -- Summary fields
    summary_additions INTEGER DEFAULT 0,
    summary_deletions INTEGER DEFAULT 0,
    summary_files INTEGER DEFAULT 0,
    summary_diffs TEXT,

    -- Revert info (JSON)
    revert TEXT,

    -- Permission ruleset (JSON)
    permission TEXT,

    -- Usage stats
    usage_input_tokens INTEGER DEFAULT 0,
    usage_output_tokens INTEGER DEFAULT 0,
    usage_reasoning_tokens INTEGER DEFAULT 0,
    usage_cache_write_tokens INTEGER DEFAULT 0,
    usage_cache_read_tokens INTEGER DEFAULT 0,
    usage_total_cost REAL DEFAULT 0.0,

    -- Status
    status TEXT NOT NULL DEFAULT 'active',

    -- Timestamps (milliseconds since epoch)
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    time_compacting INTEGER,
    time_archived INTEGER,

    -- Arbitrary session metadata (JSON object, HashMap<String, Value>)
    -- 末尾 ALTER で追加されるカラム順と揃えるため最後に置く。
    metadata TEXT
);
"#;

/// Sessions table の旧スキーマ（metadata 列なし）。
///
/// `CREATE TABLE IF NOT EXISTS` は既存テーブルに列を追加しないため、
/// `ALL_MIGRATIONS` ではこのベースライン CREATE の直後に
/// [`MIGRATE_SESSION_METADATA`] を実行する。新規 DB は ALTER で列を得るが、
/// `user_version` により ALL_MIGRATIONS は初回のみ走るため二重追加にならない。
/// 正規スキーマ（metadata 列を含む）は [`CREATE_SESSIONS_TABLE`] を参照。
pub const CREATE_SESSIONS_TABLE_LEGACY: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    parent_id TEXT,
    slug TEXT NOT NULL,
    directory TEXT NOT NULL,
    title TEXT NOT NULL,
    version TEXT NOT NULL DEFAULT '1.0.0',
    share_url TEXT,

    -- Summary fields
    summary_additions INTEGER DEFAULT 0,
    summary_deletions INTEGER DEFAULT 0,
    summary_files INTEGER DEFAULT 0,
    summary_diffs TEXT,

    -- Revert info (JSON)
    revert TEXT,

    -- Permission ruleset (JSON)
    permission TEXT,

    -- Usage stats
    usage_input_tokens INTEGER DEFAULT 0,
    usage_output_tokens INTEGER DEFAULT 0,
    usage_reasoning_tokens INTEGER DEFAULT 0,
    usage_cache_write_tokens INTEGER DEFAULT 0,
    usage_cache_read_tokens INTEGER DEFAULT 0,
    usage_total_cost REAL DEFAULT 0.0,

    -- Status
    status TEXT NOT NULL DEFAULT 'active',

    -- Timestamps (milliseconds since epoch)
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    time_compacting INTEGER,
    time_archived INTEGER
);
"#;

/// 旧 sessions テーブルに metadata 列を追加するマイグレーション。
///
/// `CREATE_SESSIONS_TABLE_LEGACY` の直後に `ALL_MIGRATIONS` へ載せ、
/// 新規／旧 DB のどちらでも起動時マイグレーション経由で列を保証する。
/// `CREATE_SESSIONS_TABLE`（metadata 列入り）を ALL_MIGRATIONS に入れると
/// 新規 DB で "duplicate column name: metadata" になるため、この ALTER を使う。
/// 旧 DB への再実行時は `database.rs` の PRAGMA 判定で既存列をスキップする
/// （v3 以降のバージョンアップでこの ALTER が二重に走らないようにするため）。
pub const MIGRATE_SESSION_METADATA: &str = "ALTER TABLE sessions ADD COLUMN metadata TEXT;";

/// Messages table - ベースライン DDL（v3 の metadata / usage 列を含まない）。
///
/// `ALL_MIGRATIONS` ではこの CREATE の直後に
/// [`MIGRATE_MESSAGES_METADATA`] / [`MIGRATE_MESSAGES_USAGE`] を実行する。
/// 新規 DB で「列を持つ CREATE」と ALTER を両方走らせると
/// "duplicate column name" になるため、基底は旧形のままにする。
/// 正規スキーマ（新列を含む）は [`CREATE_MESSAGES_TABLE_WITH_METADATA_USAGE`]。
pub const CREATE_MESSAGES_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    created_at INTEGER NOT NULL,

    -- Provider/model info
    provider_id TEXT,
    model_id TEXT,

    -- Token usage
    tokens_input INTEGER DEFAULT 0,
    tokens_output INTEGER DEFAULT 0,
    tokens_reasoning INTEGER DEFAULT 0,
    tokens_cache_read INTEGER DEFAULT 0,
    tokens_cache_write INTEGER DEFAULT 0,
    cost REAL DEFAULT 0.0,

    -- Complete message data (JSON)
    data TEXT,

    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
"#;

/// 正規の messages スキーマ（メッセージ単位の metadata / usage を含む）。
///
/// 参考用の定義であり、`ALL_MIGRATIONS` には載せない（上記ベースライン + ALTER で
/// 新規／旧 DB の両方を同じ形へ移行するため）。
pub const CREATE_MESSAGES_TABLE_WITH_METADATA_USAGE: &str = r#"
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    created_at INTEGER NOT NULL,

    -- Provider/model info
    provider_id TEXT,
    model_id TEXT,

    -- Token usage
    tokens_input INTEGER DEFAULT 0,
    tokens_output INTEGER DEFAULT 0,
    tokens_reasoning INTEGER DEFAULT 0,
    tokens_cache_read INTEGER DEFAULT 0,
    tokens_cache_write INTEGER DEFAULT 0,
    cost REAL DEFAULT 0.0,

    -- Complete message data (JSON)
    data TEXT,

    -- v3: メッセージ単位の任意メタデータ (JSON object) と使用量 (JSON object)。
    -- 旧行は NULL となり、読み取り側で空 HashMap / None として扱う。
    metadata TEXT,
    usage TEXT,

    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
"#;

/// v3: 既存 messages テーブルへメッセージ単位の metadata 列を追加する。
pub const MIGRATE_MESSAGES_METADATA: &str = "ALTER TABLE messages ADD COLUMN metadata TEXT;";

/// v3: 既存 messages テーブルへメッセージ単位の usage 列を追加する。
pub const MIGRATE_MESSAGES_USAGE: &str = "ALTER TABLE messages ADD COLUMN usage TEXT;";

/// Parts table - stores message parts (text, tool calls, etc.)
pub const CREATE_PARTS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS parts (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,

    -- Part type
    part_type TEXT NOT NULL,

    -- Text content
    text TEXT,

    -- Tool call fields
    tool_name TEXT,
    tool_call_id TEXT,
    tool_arguments TEXT,
    tool_result TEXT,
    tool_error TEXT,
    tool_status TEXT,

    -- File fields
    file_url TEXT,
    file_filename TEXT,
    file_mime TEXT,

    -- Reasoning fields
    reasoning TEXT,

    -- Sort order
    sort_order INTEGER NOT NULL DEFAULT 0,

    -- Complete part data (JSON)
    data TEXT,

    FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
"#;

/// Todos table - stores session todos
pub const CREATE_TODOS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS todos (
    session_id TEXT NOT NULL,
    todo_id TEXT NOT NULL,
    content TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    priority TEXT NOT NULL DEFAULT 'medium',
    position INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,

    PRIMARY KEY (session_id, todo_id),
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
"#;

/// Permissions table - stores project-level permissions
pub const CREATE_PERMISSIONS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS permissions (
    project_id TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    data TEXT NOT NULL
);
"#;

/// Session shares table - stores share info for sessions
pub const CREATE_SESSION_SHARES_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS session_shares (
    session_id TEXT PRIMARY KEY,
    id TEXT NOT NULL,
    secret TEXT NOT NULL,
    url TEXT NOT NULL,
    created_at INTEGER NOT NULL,

    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
"#;

/// Create indexes for better query performance
pub const CREATE_INDEXES: &str = r#"
-- Session indexes
CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project_id);
CREATE INDEX IF NOT EXISTS idx_sessions_parent ON sessions(parent_id);
CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);

-- Message indexes
CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
CREATE INDEX IF NOT EXISTS idx_messages_created ON messages(created_at);

-- Part indexes
CREATE INDEX IF NOT EXISTS idx_parts_message ON parts(message_id);
CREATE INDEX IF NOT EXISTS idx_parts_session ON parts(session_id);
CREATE INDEX IF NOT EXISTS idx_parts_order ON parts(sort_order);

-- Todo indexes
CREATE INDEX IF NOT EXISTS idx_todos_session ON todos(session_id);
CREATE INDEX IF NOT EXISTS idx_todos_status ON todos(status);
"#;

/// Session input table - stores durable admission inbox
pub const CREATE_SESSION_INPUT_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS session_input (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    prompt TEXT NOT NULL,
    delivery TEXT,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
"#;

pub const CREATE_SESSION_INPUT_TABLE_WITH_METADATA: &str = r#"
CREATE TABLE IF NOT EXISTS session_input (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    prompt TEXT NOT NULL,
    delivery TEXT,
    metadata TEXT,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE CASCADE
);
"#;

/// Adds execution metadata to existing inboxes without relying on table recreation.
pub const MIGRATE_SESSION_INPUT_METADATA: &str =
    "ALTER TABLE session_input ADD COLUMN metadata TEXT;";

pub const IDX_SESSION_INPUT: &str = r#"
CREATE INDEX IF NOT EXISTS idx_session_input_session ON session_input(session_id);
CREATE INDEX IF NOT EXISTS idx_session_input_status ON session_input(status);
"#;

/// All migration statements to run
pub const ALL_MIGRATIONS: &[&str] = &[
    CREATE_SESSIONS_TABLE_LEGACY,
    MIGRATE_SESSION_METADATA,
    CREATE_MESSAGES_TABLE,
    MIGRATE_MESSAGES_METADATA,
    MIGRATE_MESSAGES_USAGE,
    CREATE_PARTS_TABLE,
    CREATE_TODOS_TABLE,
    CREATE_PERMISSIONS_TABLE,
    CREATE_SESSION_SHARES_TABLE,
    CREATE_SESSION_INPUT_TABLE_WITH_METADATA,
    CREATE_INDEXES,
    IDX_SESSION_INPUT,
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// ALL_MIGRATIONS が作成すべきテーブル（session_input は WITH_METADATA 版のみ）。
    const EXPECTED_TABLES: &[&str] = &[
        "sessions",
        "messages",
        "parts",
        "todos",
        "permissions",
        "session_shares",
        "session_input",
    ];

    /// ベースライン DDL が宣言すべきインデックス名。
    const EXPECTED_INDEXES: &[&str] = &[
        "idx_sessions_project",
        "idx_sessions_parent",
        "idx_sessions_updated",
        "idx_sessions_status",
        "idx_messages_session",
        "idx_messages_created",
        "idx_parts_message",
        "idx_parts_session",
        "idx_parts_order",
        "idx_todos_session",
        "idx_todos_status",
        "idx_session_input_session",
        "idx_session_input_status",
    ];

    fn migrations_ddl() -> String {
        ALL_MIGRATIONS.join("\n")
    }

    fn count(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    fn table_statement<'a>(ddl: &'a str, table: &str) -> &'a str {
        let marker = format!("CREATE TABLE IF NOT EXISTS {table} (");
        let start = ddl
            .find(&marker)
            .unwrap_or_else(|| panic!("`{table}` の CREATE TABLE が見つからない"));
        let end = ddl[start..]
            .find("\n);")
            .unwrap_or_else(|| panic!("`{table}` の CREATE TABLE が閉じられていない"));
        &ddl[start..start + end]
    }

    fn declared_columns(statement: &str) -> Vec<&str> {
        statement
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("--"))
            .filter_map(|line| line.split_whitespace().next())
            .filter(|token| !matches!(*token, "CREATE" | "PRIMARY" | "FOREIGN" | ");"))
            .collect()
    }

    #[test]
    fn all_migrations_create_every_required_table_exactly_once() {
        let ddl = migrations_ddl();
        for table in EXPECTED_TABLES {
            let marker = format!("CREATE TABLE IF NOT EXISTS {table} (");
            assert_eq!(
                count(&ddl, &marker),
                1,
                "`{table}` の CREATE TABLE は 1 回だけ宣言されるべき"
            );
        }
        assert_eq!(
            count(&ddl, "CREATE TABLE"),
            EXPECTED_TABLES.len(),
            "必須テーブル以外の CREATE TABLE が混入していないこと"
        );
    }

    #[test]
    fn all_migrations_statements_are_non_empty_and_unique() {
        assert!(!ALL_MIGRATIONS.is_empty());
        for (i, statement) in ALL_MIGRATIONS.iter().enumerate() {
            assert!(
                !statement.trim().is_empty(),
                "ALL_MIGRATIONS[{i}] が空文字列"
            );
            for other in ALL_MIGRATIONS.iter().skip(i + 1) {
                assert_ne!(statement, other, "ALL_MIGRATIONS に重複した文がある");
            }
        }
    }

    #[test]
    fn required_columns_exist_in_each_table() {
        let ddl = migrations_ddl();
        let required: &[(&str, &[&str])] = &[
            (
                "sessions",
                &[
                    "id",
                    "project_id",
                    "parent_id",
                    "slug",
                    "directory",
                    "title",
                    "version",
                    "share_url",
                    "summary_additions",
                    "summary_deletions",
                    "summary_files",
                    "summary_diffs",
                    "revert",
                    "permission",
                    "usage_input_tokens",
                    "usage_output_tokens",
                    "usage_reasoning_tokens",
                    "usage_cache_write_tokens",
                    "usage_cache_read_tokens",
                    "usage_total_cost",
                    "status",
                    "created_at",
                    "updated_at",
                    "time_compacting",
                    "time_archived",
                ],
            ),
            (
                "messages",
                &[
                    "id",
                    "session_id",
                    "role",
                    "created_at",
                    "provider_id",
                    "model_id",
                    "tokens_input",
                    "tokens_output",
                    "tokens_reasoning",
                    "tokens_cache_read",
                    "tokens_cache_write",
                    "cost",
                    "data",
                ],
            ),
            (
                "parts",
                &[
                    "id",
                    "message_id",
                    "session_id",
                    "created_at",
                    "part_type",
                    "text",
                    "tool_name",
                    "tool_call_id",
                    "tool_arguments",
                    "tool_result",
                    "tool_error",
                    "tool_status",
                    "file_url",
                    "file_filename",
                    "file_mime",
                    "reasoning",
                    "sort_order",
                    "data",
                ],
            ),
            (
                "todos",
                &[
                    "session_id",
                    "todo_id",
                    "content",
                    "status",
                    "priority",
                    "position",
                    "created_at",
                    "updated_at",
                ],
            ),
            (
                "permissions",
                &["project_id", "created_at", "updated_at", "data"],
            ),
            (
                "session_shares",
                &["session_id", "id", "secret", "url", "created_at"],
            ),
            (
                "session_input",
                &[
                    "id",
                    "session_id",
                    "prompt",
                    "delivery",
                    "metadata",
                    "status",
                    "created_at",
                ],
            ),
        ];

        for (table, columns) in required {
            let statement = table_statement(&ddl, table);
            let declared = declared_columns(statement);
            for column in *columns {
                assert!(
                    declared.contains(column),
                    "`{table}` に必須カラム `{column}` が無い"
                );
            }
        }
    }

    #[test]
    fn all_migrations_declare_expected_indexes_without_duplicates() {
        let ddl = migrations_ddl();
        let mut names = Vec::new();
        for line in ddl.lines() {
            if let Some(rest) = line.trim().strip_prefix("CREATE INDEX IF NOT EXISTS ") {
                let name = rest
                    .split_whitespace()
                    .next()
                    .expect("CREATE INDEX の直後にインデックス名があること");
                names.push(name.to_string());
            }
        }

        assert_eq!(count(&ddl, "CREATE INDEX"), EXPECTED_INDEXES.len());
        assert_eq!(names.len(), EXPECTED_INDEXES.len());
        for expected in EXPECTED_INDEXES {
            assert_eq!(
                names
                    .iter()
                    .filter(|name| name.as_str() == *expected)
                    .count(),
                1,
                "インデックス `{expected}` は 1 回だけ宣言されるべき"
            );
        }
        let mut seen = HashSet::new();
        for name in &names {
            assert!(seen.insert(name.as_str()), "重複インデックス名: {name}");
        }
    }

    #[test]
    fn foreign_keys_all_use_cascade_delete() {
        let ddl = migrations_ddl();
        let foreign_key_count = count(&ddl, "FOREIGN KEY (");
        assert_eq!(foreign_key_count, 6, "全 FK が検証対象であること");
        assert_eq!(
            count(&ddl, "ON DELETE CASCADE"),
            foreign_key_count,
            "全 FK で ON DELETE CASCADE が指定されていること"
        );
    }

    #[test]
    fn session_input_baseline_matches_metadata_migration() {
        let ddl = migrations_ddl();

        // ベースラインは既に metadata 列を持つ版のみを使う。
        assert!(CREATE_SESSION_INPUT_TABLE_WITH_METADATA.contains("metadata TEXT"));
        assert!(!CREATE_SESSION_INPUT_TABLE.contains("metadata"));
        assert_eq!(count(&ddl, "CREATE TABLE IF NOT EXISTS session_input ("), 1);
        assert!(ALL_MIGRATIONS.contains(&CREATE_SESSION_INPUT_TABLE_WITH_METADATA));
        assert!(!ALL_MIGRATIONS.contains(&CREATE_SESSION_INPUT_TABLE));

        // 旧スキーマ用 ALTER は ALL_MIGRATIONS に含めない。
        // database.rs が PRAGMA table_info で列の有無を判定して実行するため。
        assert_eq!(
            MIGRATE_SESSION_INPUT_METADATA,
            "ALTER TABLE session_input ADD COLUMN metadata TEXT;"
        );
        assert!(!ALL_MIGRATIONS.contains(&MIGRATE_SESSION_INPUT_METADATA));
    }

    /// sessions.metadata は正規 CREATE に宣言しつつ、ALL_MIGRATIONS では
    /// ベースライン CREATE + ALTER の 2 段で適用する（新規 DB での二重追加回避）。
    #[test]
    fn sessions_metadata_is_declared_canonically_and_migrated_at_startup() {
        // 正規スキーマには metadata 列がある。
        assert!(CREATE_SESSIONS_TABLE.contains("metadata TEXT"));
        assert!(!CREATE_SESSIONS_TABLE_LEGACY.contains("metadata"));

        // ALL_MIGRATIONS はベースライン CREATE と ALTER をこの順で 1 回ずつ持つ。
        assert!(ALL_MIGRATIONS.contains(&CREATE_SESSIONS_TABLE_LEGACY));
        assert!(!ALL_MIGRATIONS.contains(&CREATE_SESSIONS_TABLE));
        assert!(ALL_MIGRATIONS.contains(&MIGRATE_SESSION_METADATA));
        assert_eq!(
            MIGRATE_SESSION_METADATA,
            "ALTER TABLE sessions ADD COLUMN metadata TEXT;"
        );
        let create_index = ALL_MIGRATIONS
            .iter()
            .position(|statement| *statement == CREATE_SESSIONS_TABLE_LEGACY)
            .expect("legacy sessions CREATE is in ALL_MIGRATIONS");
        let alter_index = ALL_MIGRATIONS
            .iter()
            .position(|statement| *statement == MIGRATE_SESSION_METADATA)
            .expect("sessions metadata ALTER is in ALL_MIGRATIONS");
        assert!(
            create_index < alter_index,
            "ALTER はベースライン CREATE の後に実行されること"
        );
        assert_eq!(
            count(&migrations_ddl(), "CREATE TABLE IF NOT EXISTS sessions ("),
            1
        );
    }

    /// messages の metadata / usage は正規 CREATE に宣言しつつ、
    /// ALL_MIGRATIONS ではベースライン CREATE + ALTER の 2 段で適用する
    /// （sessions.metadata と同じ方式。新規 DB での二重追加を避ける）。
    #[test]
    fn messages_metadata_and_usage_are_declared_canonically_and_migrated_at_startup() {
        assert!(CREATE_MESSAGES_TABLE_WITH_METADATA_USAGE.contains("metadata TEXT"));
        assert!(CREATE_MESSAGES_TABLE_WITH_METADATA_USAGE.contains("usage TEXT"));
        assert!(!CREATE_MESSAGES_TABLE.contains("metadata TEXT"));
        assert!(!CREATE_MESSAGES_TABLE.contains("usage TEXT"));

        let canonical_columns = declared_columns(table_statement(
            CREATE_MESSAGES_TABLE_WITH_METADATA_USAGE,
            "messages",
        ));
        assert!(canonical_columns.contains(&"metadata"));
        assert!(canonical_columns.contains(&"usage"));

        assert!(ALL_MIGRATIONS.contains(&CREATE_MESSAGES_TABLE));
        assert!(!ALL_MIGRATIONS.contains(&CREATE_MESSAGES_TABLE_WITH_METADATA_USAGE));
        assert_eq!(
            MIGRATE_MESSAGES_METADATA,
            "ALTER TABLE messages ADD COLUMN metadata TEXT;"
        );
        assert_eq!(
            MIGRATE_MESSAGES_USAGE,
            "ALTER TABLE messages ADD COLUMN usage TEXT;"
        );
        assert!(ALL_MIGRATIONS.contains(&MIGRATE_MESSAGES_METADATA));
        assert!(ALL_MIGRATIONS.contains(&MIGRATE_MESSAGES_USAGE));

        let create_index = ALL_MIGRATIONS
            .iter()
            .position(|statement| *statement == CREATE_MESSAGES_TABLE)
            .expect("baseline messages CREATE is in ALL_MIGRATIONS");
        let metadata_index = ALL_MIGRATIONS
            .iter()
            .position(|statement| *statement == MIGRATE_MESSAGES_METADATA)
            .expect("messages metadata ALTER is in ALL_MIGRATIONS");
        let usage_index = ALL_MIGRATIONS
            .iter()
            .position(|statement| *statement == MIGRATE_MESSAGES_USAGE)
            .expect("messages usage ALTER is in ALL_MIGRATIONS");
        assert!(
            create_index < metadata_index && metadata_index < usage_index,
            "ALTER はベースライン CREATE の後に実行されること"
        );
        assert_eq!(
            count(&migrations_ddl(), "CREATE TABLE IF NOT EXISTS messages ("),
            1
        );
    }

    #[test]
    fn schema_ddl_does_not_manage_user_version() {
        // user_version の記録は database.rs の SCHEMA_VERSION が担う。
        // 0 のままだとマイグレーションが毎回実行されてしまう。
        const { assert!(crate::database::SCHEMA_VERSION > 0) };

        for statement in ALL_MIGRATIONS
            .iter()
            .chain(std::iter::once(&MIGRATE_SESSION_INPUT_METADATA))
            .chain(std::iter::once(&MIGRATE_MESSAGES_METADATA))
            .chain(std::iter::once(&MIGRATE_MESSAGES_USAGE))
        {
            assert!(
                !statement.to_ascii_lowercase().contains("user_version"),
                "スキーマ DDL に user_version 操作を含めないこと"
            );
        }
    }
}

---
type: Reference
description: SQLite database、schema migration、repository を説明する。
tags: [storage, sqlite, persistence, crate]
---

# opencode-storage

`opencode-storage` provides SQLite persistence: database setup and repository access.

## Responsibilities

- Database connection and migrations
- Session, message, todo, and durable session-input repository implementations
- Unified storage error model

## Module structure

- `database.rs` – DB init, connection management
- `schema.rs` – Table definitions and migrations
- `repository.rs` – Session/Message/Todo repositories

## Key exports

- `Database`
- `DatabaseError`
- `SessionRepository`
- `MessageRepository`
- `TodoRepository`
- `InputRepository`

## Development notes

- Schema changes must remain compatible across migrations
- Keep transaction boundaries clear in repository APIs
- Consider indexes and query cost for hot read/write paths
- Session input rows use `pending`, `processing`, `promoted`, and `failed`
  states; claims enforce per-session FIFO ordering in the database

See [ADR-0004](adr/adr-0004-durable-session-input-queue.md) for queue
recovery semantics and the single-server-per-database constraint.

## Database path resolution

`Database::database_path()` is the single source of truth for the active
database path. It resolves as follows:

1. If `OPENCODE_DATA_DIR` is set to a non-empty value, its trimmed value
   replaces `dirs::data_local_dir()` as the base directory.
2. Otherwise the base directory is `dirs::data_local_dir()`. Empty,
   whitespace-only, and non-UTF-8 values are ignored.
3. The database file is `<base>/opencode/opencode.db`.

`opencoderust db path` prints this same resolved path. Use it to confirm
which file a backup must cover before changing `OPENCODE_DATA_DIR` or
starting a new server process.

## Backup and recovery runbook

The SQLite database (by default
`${XDG_DATA_HOME:-~/.local/share}/opencode/opencode.db`, or
`<OPENCODE_DATA_DIR>/opencode/opencode.db` when that variable is set) is the
durable source of truth for sessions, messages, todos, and the
session-input queue. Back it up with SQLite's online facilities only;
never copy a live `.db` file with `cp`.

### Prerequisites

- Determine the active database path with `opencoderust db path` (or
  `Database::database_path()`) before taking a backup. Check
  `OPENCODE_DATA_DIR` in the server's environment as well.
- One database must be owned by exactly one storage-backed `ServerState`
  (one server process). See
  [ADR-0004](adr/adr-0004-durable-session-input-queue.md). Stop or isolate
  any second server before restoring; its in-memory session snapshots can
  otherwise overwrite the restored state.
- Store backups outside the database directory, on a separate filesystem
  or host.

### Online backup

Both SQLite online methods produce a consistent snapshot without stopping
the writer.

```bash
# VACUUM INTO: writes a compact, self-contained file; target must not exist
sqlite3 /path/to/opencode.db \
  "VACUUM INTO '/var/backups/opencode/opencode-20261120.db';"

# .backup: SQLite online backup API; overwrites the target file
sqlite3 /path/to/opencode.db \
  ".backup '/var/backups/opencode/opencode-20261120.db'"
```

`VACUUM INTO` fails if the destination already exists, so use a unique
timestamped name. For `.backup`, write to a staging path and rename the
file atomically after the command succeeds.

### WAL and -shm sidecars

The storage crate does not set `journal_mode` explicitly; it uses the sqlx
default unless the database has been switched to WAL. If
`PRAGMA journal_mode` returns `wal`, the database has `-wal` and `-shm`
sidecars.

- Both online methods above include committed WAL content in the backup,
  so the backup file needs no `-wal`/`-shm` pair of its own.
- Do not back up the `.db` file alone with `cp` while WAL is active:
  committed transactions may still live only in `-wal`.
- Never delete `-wal`/`-shm` from a live database directory. Stop the
  server cleanly first so SQLite checkpoints and removes the sidecars.
- For a cold copy, either stop the server and copy after a clean checkpoint,
  or restore `.db`, `-wal`, and `-shm` together.

### Restore

1. Stop the storage-backed server process to preserve the single-writer
   constraint from ADR-0004.
2. Move the current database aside for rollback, then place the backup at
   the resolved database path (`opencoderust db path`).

### Post-restore integrity verification

Verify the restored file before restarting the server:

```bash
sqlite3 /path/to/opencode.db "PRAGMA integrity_check;"
sqlite3 /path/to/opencode.db "PRAGMA user_version;"
```

`integrity_check` must return `ok`. `user_version` records the applied
schema level; this codebase writes `SCHEMA_VERSION = 2`
(`crates/opencode-storage/src/database.rs`). A lower value means the
storage layer applies migrations at startup. A higher value means the
backup came from a newer binary and must not be used.

Restart the server afterwards. At storage-backed startup, stale
`processing` queue rows are reset to `pending`, so interrupted work
resumes (ADR-0004).

### Frequency and destination example

- Run an online backup hourly, plus one immediately before a schema
  migration or binary upgrade.
- Write to `/var/backups/opencode/` on a separate filesystem, and copy the
  daily snapshot to off-host storage.
- Keep 24 hourly and 30 daily snapshots, and test a restore quarterly.
- Never overwrite the most recent known-good backup.

## Validation

```bash
cargo check -p opencode-storage
```

---
type: Project
description: OpenCodeRust の概要、使い方、workspace 構成への入口。
tags: [project, rust, opencode]
---

# OpenCodeRust

A Rust implementation of OpenCode

OpenCodeRust is the Rust implementation and evolution of OpenCode. It provides
a full CLI/TUI/Server workflow for local AI coding agents, session management,
tool invocation, MCP/LSP integration, and plugin extensions.

## Current status

- Project name: `OpenCodeRust`
- Executable: `opencoderust`

## Feature overview

- **Interaction modes:** TUI (default), CLI single run, HTTP server, Web/ACP mode
- **Sessions:** Create, continue, fork sessions; import/export
- **Tool system:** Built-in read/write/edit, shell, patch, and related tools
- **Model support:** Multiple providers, agent mode switching
- **Extensibility:** Plugin bridge (including TS plugins), MCP, LSP
- **Terminal:** Improved layout, collapsible sidebar, syntax highlighting,
  path completion

## Quick start

### 1. Requirements

- Rust 1.97 or later (MSRV declared as `rust-version` in the workspace root `Cargo.toml`)
- Cargo
- C compiler and build tools (`gcc` and `make`; also `pkg-config` when linking
  against a system SQLite instead of the bundled build)
- Git (recommended)
- `sqlite3` CLI (external dependency of the `db` command and the
  backup/restore procedures in `docs/opencode-storage.md`)
- GitHub CLI (`gh`) (external dependency of the `github` and `pr` commands)

### 2. Build

```bash
cargo build -p opencode-cli
```

### 3. Use this project's binary

To ensure you are running this project's binary, run from the **repo root**:

- **`./target/debug/opencoderust`** after `cargo build -p opencode-cli`
- **`cargo run -p opencode-cli --`** to always use this repo's version

### 4. Show help

```bash
./target/debug/opencoderust --help
```

or

```bash
cargo run -p opencode-cli -- --help
```

### 5. How to run

- Default: start TUI:

```bash
cargo run -p opencode-cli --
```

- Start TUI explicitly:

```bash
cargo run -p opencode-cli -- tui
```

- Single non-interactive run:

```bash
cargo run -p opencode-cli -- run "Check this repo for risks"
```

- Start HTTP server:

```bash
cargo run -p opencode-cli -- serve --port 3000 --hostname 127.0.0.1
```

## CLI commands overview

From the repo root use `./target/debug/opencoderust` or
`cargo run -p opencode-cli --`. These commands match the current
`./target/debug/opencoderust --help`:

- `tui` – Start interactive terminal UI
- `attach` – Attach to a running server
- `run` – Run a single message
- `serve` – Start HTTP server
- `web` – Start headless server and open web UI
- `acp` – Start ACP server
- `models` – List available models
- `session` – Session management
- `stats` – Token/cost statistics
- `db` – Database tools
- `config` – Show configuration
- `auth` – Credential management
- `agent` – Agent management
- `debug` – Debugging and troubleshooting
- `mcp` – MCP management
- `export` / `import` – Export/import sessions
- `github` / `pr` – GitHub-related features
- `upgrade` / `uninstall` – Upgrade and uninstall
- `generate` – Generate OpenAPI spec
- `version` – Show version

Subcommand help:

```bash
./target/debug/opencoderust tui --help
./target/debug/opencoderust run --help
./target/debug/opencoderust serve --help
./target/debug/opencoderust session --help
```

## Keyboard and input

The TUI does not support text entry through an IME (input method editor).
To enter Japanese or other non-ASCII text, copy it to the clipboard first
and paste it into the prompt with `Ctrl+V`.

`Ctrl+X` is a leader key: press it, then one of `n` / `l` / `m` / `a` /
`t` / `b` / `s` / `q` / `u` / `r`. The prompt status line lists the
available keys while it waits for the next key. Press `Esc` to cancel.

## Configuration

Configuration sources are merged in the following order, from lowest to
highest precedence. Each later source overrides earlier ones; the managed
source is merged last and therefore has the highest priority.

| Order | Source | Details |
| --- | --- | --- |
| 1 (lowest) | Remote well-known | `.well-known/opencode` org defaults |
| 2 | Global | `~/.config/opencode/opencode.json{,c}` |
| 3 | Custom | Path from `OPENCODE_CONFIG` |
| 4 | Project | `opencode.json{,c}` found by upward search (see note) |
| 5 | `.opencode` dirs | Agents, commands, plugins, modes (markdown) |
| 6 | Inline | `OPENCODE_CONFIG_CONTENT` |
| 7 | Env overrides | `OPENCODE_PERMISSION`, `OPENCODE_DISABLE_*` |
| 8 (highest) | Managed | Enterprise config directory, merged last |

At each project level both `opencode.jsonc` and `opencode.json` are searched.
The project layer also reads `$HOME/.opencode/opencode.json{,c}` and
`$OPENCODE_CONFIG_DIR/opencode.json{,c}` explicitly.
`.opencode/opencode.json{,c}` are read by the project find-up scan, so the
`.opencode` directory scan itself only loads agents, commands, plugins and
modes from markdown/JS files. Environment overrides are applied before the
managed config so that managed stays the highest-priority source.

Relative file references (`{file:...}`) in the inline source
(`OPENCODE_CONFIG_CONTENT`) resolve against the process current working
directory, not the parent directory of the `OPENCODE_CONFIG` file.

See: `docs/opencode-config.md`

Runtime changes through `PATCH /config` take effect immediately for most keys.
`logLevel` / `log_level` and `server.*` bind keys are read only at startup and
require a restart, as do the environment variables listed in
[Logging](#logging).

### Invalid or problematic configuration

Run `opencoderust config --check` to validate the current configuration. It
prints `Configuration is valid` on success and exits non-zero on errors.

Most configuration problems produce a warning and startup continues instead
of failing. Review each warning as follows.

- Unknown keys (top level or nested): a warning is logged and startup
  continues. User-defined names are masked with an asterisk. Check the
  warned key path and remove or rename the misspelled key.
- Wrong value types: only the field path is warned, never the value. Check
  that field's type in the config reference and correct the value.
- Undefined `{env:VAR}` placeholders: expanded to an empty string with a
  warning (this is not fail-closed). Confirm the variable is set in the
  environment, or replace the placeholder with a literal value.
- Deprecated keys `mode`, `tools`, `autoshare`: accepted with a migration
  warning. Move to the current equivalent key and drop the old one.
- Explicit `OPENCODE_CONFIG` path missing: a warning names the file only.
  Create the file or point the variable at an existing config file.
- MCP disk sync failure: an `x-mcp-sync-warning` header and a fixed warning
  are emitted. Check disk space and write permissions for the MCP state
  directory, then retry.
- `logLevel` / `log_level`, `server.*` bind keys, and environment variables:
  read only at startup. Restart the process after changing them.

## Operations

### Logging

- Log level resolution: config `log_level` (when set) -> `RUST_LOG` -> `info`.
  Use `RUST_LOG` (for example `RUST_LOG=debug`) to raise verbosity without
  editing config files.
- TUI sessions write logs to a file. Non-TUI commands (`run`, `serve`, and
  other subcommands) print to stdout/stderr and do not create a log file.
  Exception: `run --format json` creates a file log so stdout stays a clean
  JSON Lines stream.
- Default log directory (Unix): `$XDG_STATE_HOME/opencode/logs`, or
  `$HOME/.local/state/opencode/logs` when `XDG_STATE_HOME` is unset. If `HOME`
  is unavailable too, `./.opencode/logs` is used. Each launch writes a
  `<YYYY-MM-DDTHHMMSS>-<pid>-<nanoseconds>.log` file; the suffix prevents
  truncation when two processes start in the same second.
- Seven generations (the current file plus six previous ones) are kept by
  modification time. Override with `OPENCODE_LOG_RETENTION` (positive integer;
  invalid or zero falls back to 7).
- Log files are created with mode `0600`; pre-existing files with looser
  permissions are corrected to `0600` on open. If a log file cannot be opened,
  events fall back to stderr instead of being silently dropped.
- If a log file is missing or unreadable, check the directory above (or the
  path reported at startup) and set `XDG_STATE_HOME` explicitly to redirect
  logs to a known location. Buffered entries are flushed when the process
  exits normally.

`PATCH /config` applies most settings immediately, but some keys are read only
at startup. A successful PATCH response lists them under `restart_required`:

| Setting | `PATCH /config` | Restart needed |
| --- | --- | --- |
| Providers, models, permissions, and other runtime keys | Immediate | No |
| `server.*` bind keys (port, hostname, cors, mdns) | Not applied | Yes |
| `logLevel` / `log_level` | Not applied | Yes |
| Environment variables (see note) | Not a PATCH key | N/A |

Environment variables are not configuration keys, so they never appear in
`restart_required`. The following are resolved once when the process starts;
changing them while it runs has no effect and requires a restart:

- `RUST_LOG` – log level filter (resolution order: config `log_level`, then
  `RUST_LOG`, then `info`)
- `OPENCODE_LOG_RETENTION` – number of log generations to keep
- `OPENCODE_NO_AUTH` – whether authentication is disabled
- `OPENCODE_AUTO_SHARE` – `true`/`1`/`yes` (case-insensitive) triggers auto-sharing
  for `run --attach`, right after the session is created (before the first
  message); `on` is false. No effect on local `run`. Same as `config.share: auto`.
- `OPENCODE_DATA_DIR` – base directory for the database and other local data

`logLevel` / `log_level` and `server.*` bind keys are likewise read only at
startup, as shown in the table above.

### Health checks

Two probes are exposed. Both `/health` and `/global/health` return the same
JSON schema:

```json
{"status":"ok","version":"<version>","healthy":true}
```

The existing `status` and `version` fields are kept for compatibility; the
`healthy` field mirrors `status == "ok"` and was previously only present on
`/global/health`. Liveness probes are deliberately exempt from the
authentication middleware so the CLI and TUI can wait for startup without a
token. Readiness probes and all other routes still require the startup Bearer
token when authentication is enabled.

#### Liveness

`GET /health` (aliases: `GET /health/live`, `GET /global/health`) is a pure
liveness probe. It always returns `200` with `status: "ok"` while the process
can serve HTTP; it does not inspect the database. These three paths match
exactly (query strings are ignored), and none of them requires an
`Authorization` header. Only these three root paths are exempt; the `/api`
aliases (for example `/api/health`) still require authentication, so use the
root paths for unauthenticated probes. The CLI and TUI use `/health` without a
token to wait for a local server to start:

```bash
curl -fsS http://127.0.0.1:3000/health
```

Use liveness only to decide whether to restart a process. Never use it as a
traffic or dependency gate.

#### Readiness

`GET /health/ready` (alias: `GET /global/ready`) probes the database with a
lightweight `SELECT 1` through the configured repository. It returns:

- `200` with `status: "ok"` when the database is reachable and usable.
- `503` with `status: "error"` and `healthy: false` when the repository is not
  initialized, the connection is closed, the database is locked or the stored
  data is corrupt.

Database migrations are applied at startup. The probe only checks connectivity
and basic usability with `SELECT 1`; it does not verify migration status.

```bash
curl -fsS -o /dev/null -w '%{http_code}\n' \
  http://127.0.0.1:3000/health/ready
```

Unlike liveness, `/health/ready` and `/global/ready` pass through the
authentication middleware. Send `Authorization: Bearer <token>` when
authentication is enabled. Point load-balancer and orchestrator readiness gates
at `/health/ready`, not at `/health`. A static `200` from `/health` no longer
routes traffic to a server whose database is unavailable.

Replace the host and port with the values passed to `serve --hostname` and
`--port`.

### Authentication

- Authentication is enabled by default. A random Bearer token (32 bytes,
  URL-safe base64) is generated at startup; send it on every request as
  `Authorization: Bearer <token>`, except for the liveness probes documented
  above. The token value is masked in logs, and the startup notice on stderr
  reports whether authentication is enabled.
- The token is regenerated on every restart. Clients must refresh the
  `Authorization` header after restarting the server.
- `OPENCODE_NO_AUTH` disables authentication only when set to `true`, `1`, or
  `yes` (case-insensitive, trimmed). Any other value, including unset, keeps
  authentication enabled (fail-closed). Do not disable it outside local
  development.

## Repository structure

The complete 21-crate map and major execution paths are documented in
[`docs/architecture.md`](docs/architecture.md).

- `crates/opencode-cli` – CLI entrypoint (binary: `opencoderust`)
- `crates/opencode-server` – HTTP/SSE/WebSocket server
- `crates/opencode-tui` – Terminal UI
- `crates/opencode-session` – Sessions and messages
- `crates/opencode-tool` – Tool registration and execution
- `crates/opencode-provider` – Model provider adapters
- `crates/opencode-plugin` – Plugin system and subprocess bridge
- `crates/opencode-mcp` – MCP client and registration
- `crates/opencode-lsp` – LSP support
- `crates/opencode-storage` – SQLite storage

## Development and validation

CI is defined in `.github/workflows/ci.yml` and delegated to the shared
`Seduce0068/workflows/.github/workflows/rust-gates.yml@v1` workflow.
`Cargo.lock` is committed, so cargo commands use `--locked`; a `cargo audit`
gate is also enabled. The four `extra_gates` below mirror the CI job exactly.

```bash
cargo check --workspace --all-features --all-targets --locked
cargo test --workspace --all-features --all-targets --locked
cargo clippy --workspace --all-features --all-targets --locked -- \
  -D warnings -D clippy::undocumented_unsafe_blocks
cargo build --workspace --release --locked
```

The default-feature gates (fmt / check / test / clippy) also run with
`locked: true`, `audit: true`, and
`clippy_args: -D clippy::undocumented_unsafe_blocks`, so `-D warnings` and
the unsafe-block lint apply to both feature sets.

`cargo nextest run --workspace` remains available as a faster local test
runner; CI uses `cargo test` (see the gates above).

Minimal check (typical):

```bash
cargo check -p opencode-cli
cargo check -p opencode-tui
```

## Documentation

- Project architecture: `docs/architecture.md`
- User guide: `USER_GUIDE.md`
- Docs index: `docs/README.md`
- CLI: `docs/opencode-cli.md`
- TUI: `docs/opencode-tui.md`
- Server: `docs/opencode-server.md`
- Tools: `docs/opencode-tool.md`
- Provider: `docs/opencode-provider.md`
- Config: `docs/opencode-config.md`
- Storage and backups: `docs/opencode-storage.md`

## Notes

- The executable name is `opencoderust`. OpenCodeRust is the project name.

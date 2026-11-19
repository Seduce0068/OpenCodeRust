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

- Rust stable
- Cargo
- Git (recommended)

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

## Configuration

Configuration is merged from the following paths in priority order (searched upward):

- `opencode.jsonc`
- `opencode.json`
- `.opencode/opencode.jsonc`
- `.opencode/opencode.json`

Global config default path:

- Linux/macOS: `~/.config/opencode/opencode.jsonc` (or `.json`)

See: `docs/opencode-config.md`

## Repository structure

The complete 22-crate map and major execution paths are documented in
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

## Notes

- The executable name is `opencoderust`. OpenCodeRust is the project name.

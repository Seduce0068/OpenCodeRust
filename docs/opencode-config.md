---
type: Reference
description: 設定ファイルの発見、解析、型付け、merge を説明する。
tags: [configuration, jsonc, crate]
---

# opencode-config

`opencode-config` handles config file discovery, loading, parsing, and merging.
It is the configuration entrypoint for runtime behaviour.

## Responsibilities

- Search for config files (project and global)
- Parse JSON/JSONC (comments supported)
- Map config to strongly typed structures
- Provide well-known paths and defaults

## Module structure

- `loader.rs` – Config loading, path lookup, merge flow
- `schema.rs` – Config structure definitions
- `wellknown.rs` – Common directory/file path constants

## Config paths (common)

- Project: `opencode.jsonc` / `opencode.json`
- Project extension: `.opencode/opencode.jsonc` / `.opencode/opencode.json`
- Global: `~/.config/opencode/opencode.jsonc` (or `.json`)

## Usage notes

- When adding new config fields, define default behaviour
- Merge behaviour should remain predictable
- Changes to provider/mcp/agent fields should be reflected in docs

## Schema compatibility

The loader accepts both shapes of the top-level `permissions` list:

- Legacy: `"permissions": ["read", "edit"]`
- Current: `"permissions": [{"action": "read", "resource": ".env", "effect": "deny"}]`

`permissions` is informational for this implementation; enforcement uses the
`permission` field. Unknown top-level keys are ignored by serde, so a config
written for a newer OpenCode schema does not block startup.

The current-schema `providers` map is normalized into the runtime `provider`
map when `load_all` runs:

- `package` → `npm`
- `settings.baseURL` / `settings.apiKey` → `base_url` / `api_key`
- `headers` / `compatibility` → `options.headers` / `options.compatibility`
- `models.<id>.limit` / `capabilities` → runtime model limits and capabilities
- `modelID` is accepted as an alias of the model `model` field

A legacy `provider` entry for the same provider id wins over the normalized
current-schema entry.

The current-schema `mcp.servers` wrapper is unwrapped into the runtime `mcp`
map: `disabled` is inverted to `enabled`, `command` accepts a string or an argv
array, and `headers` / `environment` / `oauth: false` are carried over. The
per-operation `timeout` object and `cwd` are not mapped yet.

The following keys are accepted but not mapped to runtime behaviour yet
(see [MAPPING](../MAPPING.md)):

- `snapshots`, `websearch`, `shell`

Tests use `ConfigLoader::without_global_config()` so they do not depend on the
machine's `~/.config/opencode`.

## Validation

```bash
cargo check -p opencode-config
cargo test -p opencode-config
```

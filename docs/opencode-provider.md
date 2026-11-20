---
type: Reference
description: provider adapter、model 解決、streaming、retry を説明する。
tags: [provider, models, crate]
verified:
  by: agent
  at: "2026-11-16T23:19:05+09:00"
---

# opencode-provider

`opencode-provider` is the multi-provider model adapter layer: request
building, streaming response parsing, retries, and model capability queries.

## Responsibilities

- Unified provider abstraction
- Integrate multiple vendor APIs (OpenAI, Anthropic, Google, xAI, Groq, etc.)
- Do not register a local LiteRT-LM provider (FFI 未リンクのため登録しない /
  FFI is not linked; see below)
- Model registration, context window, capability metadata
- Retry and streaming event handling

## Key modules

- `provider.rs` – Provider trait and unified call entry
- `bootstrap.rs` – Build registry from config/env
- `models.rs` – Model metadata and capability queries
- `stream.rs` – Streaming event abstraction
- `retry.rs` – Retry policy and retryability
- `<vendor>.rs` – Per-vendor implementations
- `litert.rs` – Local LiteRT-LM provider (not yet linked)

## Key exports

- `create_registry_from_bootstrap_config`
- `create_registry_from_env`
- `with_retry` / `with_retry_and_hook`
- `get_model_context_limit`

## LiteRT-LM provider (local, incomplete)

The `litert` provider is **not implemented**: FFI to the LiteRT-LM C++
library is not linked. `chat` and `chat_stream` return an explicit
"not implemented" error, and `bootstrap.rs` does **not** register the
provider, even when `modelPath` / `LITERT_MODEL_PATH` and `backend` /
`LITERT_BACKEND` are configured. Those environment variables are ignored
until the FFI is implemented. There is no feature gate; the `litert` module
is always compiled as a placeholder.

In this environment the library is absent from `pkg-config` and `ldconfig`,
so an FFI implementation needs the native dependency first. After FFI is
linked, the `litert` branch in `create_concrete_provider` must be
re-enabled before the provider is advertised.

## Relations to other modules

- Used by `opencode-session`, `opencode-agent`, `opencode-cli`
- Integrates with `opencode-plugin` hooks (e.g. request params/headers modification)

## Validation

```bash
cargo check -p opencode-provider
```

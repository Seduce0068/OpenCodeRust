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
- Register a local LiteRT-LM provider (currently a placeholder; see below)
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

`bootstrap.rs` creates a `litert` provider from `modelPath` / `model_path`
(or `LITERT_MODEL_PATH`) and `backend` (or `LITERT_BACKEND`, default `npu`).
It advertises the `gemma-4-e2b` model with a 262,144-token context and
4,096 max output tokens.

`chat` and `chat_stream` are not linked to the LiteRT-LM C++ library. They
return `ProviderError::ApiError` placeholders, so the provider is unavailable
for inference. In this environment the library is absent from `pkg-config`
and `ldconfig`, so an FFI implementation needs the native dependency first.

## Relations to other modules

- Used by `opencode-session`, `opencode-agent`, `opencode-cli`
- Integrates with `opencode-plugin` hooks (e.g. request params/headers modification)

## Validation

```bash
cargo check -p opencode-provider
```

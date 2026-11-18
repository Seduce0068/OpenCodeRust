//! 複数の LLM プロバイダへの接続とモデル情報の管理を提供するクレート。
//!
//! `Provider` トレイトを Anthropic / OpenAI / Google / Bedrock などの各サービスが
//! 実装し、`ProviderRegistry` が ID 単位で登録・解決する。`models` は models.dev
//! 由来のモデル情報（コンテキスト長・コストなど）を保持し、`auth` / `retry` /
//! `stream` / `transform` が認証・再試行・ストリーミング・メッセージ変換を担う。
//!
//! # 使い方
//!
//! `create_default_registry()` でレジストリを組み立て、目的のプロバイダを
//! 取得してチャットリクエストを送信する。

#![allow(
    clippy::field_reassign_with_default,
    clippy::if_same_then_else,
    clippy::unnecessary_sort_by,
    clippy::too_many_arguments,
    clippy::needless_range_loop
)]

pub mod anthropic;
pub(crate) mod auth;
pub mod azure;
pub mod bedrock;
pub mod bootstrap;
pub mod cerebras;
pub mod cohere;
pub(crate) mod custom_fetch;
pub mod deepinfra;
pub mod deepseek;
pub mod github_copilot;
pub mod gitlab;
pub mod google;
pub mod groq;
pub mod litert;
pub(crate) mod message;
pub mod mistral;
pub mod models;
pub mod openai;
pub mod openrouter;
pub mod perplexity;
pub(crate) mod provider;
pub mod responses;
pub mod responses_convert;
pub mod retry;
pub(crate) mod stream;
pub mod together;
pub(crate) mod tools;
pub mod transform;
pub mod vercel;
pub mod vertex;
pub mod xai;

pub use auth::*;
pub use bootstrap::create_registry_from_env;
pub use bootstrap::create_registry_from_env_with_auth_store;
pub use bootstrap::{
    apply_custom_loaders, bootstrap_config_from_raw, create_registry_from_bootstrap_config,
    filter_models_by_status, BootstrapConfig, ConfigModel, ConfigProvider, CustomLoaderResult,
};
pub use custom_fetch::*;
pub use message::*;
pub use provider::*;
pub use retry::{with_retry, with_retry_and_hook, IsRetryable, RetryConfig};
pub use stream::*;
pub use tools::*;
pub use transform::{
    apply_caching, apply_caching_per_part, extract_reasoning_from_response, max_output_tokens,
    mime_to_modality, normalize_interleaved_thinking, normalize_messages,
    normalize_messages_for_caching, normalize_messages_with_interleaved_field, options,
    provider_options_map, schema, sdk_key, small_options, temperature_for_model, top_k_for_model,
    top_p_for_model, transform_messages, unsupported_parts, variants, Modality, ProviderType,
    OUTPUT_TOKEN_MAX,
};

pub use models::{
    get_model_context_limit, supports_function_calling, supports_vision, ModelCost,
    ModelInfo as ModelsDevInfo, ModelLimit, ModelModalities, ModelsData, ModelsRegistry,
    ProviderInfo as ModelsProviderInfo,
};

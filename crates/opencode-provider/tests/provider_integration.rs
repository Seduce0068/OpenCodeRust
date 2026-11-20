//! Anthropic SSE の行分割回帰テストを含む provider 統合テスト。
//!
//! ## 実行上の注意（ループバックモック）
//!
//! 本ファイルの `anthropic_stream_*` テストは `127.0.0.1` の 1 ショット HTTP
//! モックを使う。各テスト冒頭の `ensure_no_proxy()` がプロセス内の `NO_PROXY` /
//! `no_proxy` にループバックを追加し、テスト終了時に元値へ復元するため、
//! 実行者の `HTTP_PROXY` / `HTTPS_PROXY` / `NO_PROXY` 設定に依存せず動作する。
//! 送信先はループバックのモックのみで、外部送信は行わない。

use futures::StreamExt;
use opencode_provider::{
    anthropic::{AnthropicConfig, AnthropicProvider},
    ChatRequest, Message, ModelInfo, Provider, ProviderRegistry, StreamEvent,
};

/// 環境変数の書き換えをテスト間で直列化するロック。
/// `std::sync::MutexGuard` を `.await` を跨いで保持すると clippy の
/// `await_holding_lock` に抵触するため、tokio の非同期 Mutex を使う。
static NO_PROXY_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// `ensure_no_proxy()` が保存した元値を `Drop` で復元するガード。
/// ロックも保持するため、ガードの生存期間中は他テストが env を書き換えない。
struct EnsureNoProxyGuard {
    no_proxy: Option<String>,
    lowercase_no_proxy: Option<String>,
    _lock: tokio::sync::MutexGuard<'static, ()>,
}

fn restore_env(key: &str, original: Option<&str>) {
    match original {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
}

impl Drop for EnsureNoProxyGuard {
    fn drop(&mut self) {
        restore_env("NO_PROXY", self.no_proxy.as_deref());
        restore_env("no_proxy", self.lowercase_no_proxy.as_deref());
    }
}

fn set_no_proxy_with_loopback(key: &str, original: Option<&str>) {
    const LOOPBACK: &str = "127.0.0.1,localhost";
    let value = match original {
        Some(original) if !original.trim().is_empty() => format!("{original},{LOOPBACK}"),
        _ => LOOPBACK.to_string(),
    };
    std::env::set_var(key, value);
}

/// ループバック接続がプロキシを経由しないよう `NO_PROXY` / `no_proxy` を設定する。
/// 元値（未設定を含む）を保存して `Drop` で復元するので、実行者の設定に依存しない。
async fn ensure_no_proxy() -> EnsureNoProxyGuard {
    let lock = NO_PROXY_ENV_LOCK.lock().await;
    let no_proxy = std::env::var("NO_PROXY").ok();
    let lowercase_no_proxy = std::env::var("no_proxy").ok();
    set_no_proxy_with_loopback("NO_PROXY", no_proxy.as_deref());
    set_no_proxy_with_loopback("no_proxy", lowercase_no_proxy.as_deref());
    EnsureNoProxyGuard {
        no_proxy,
        lowercase_no_proxy,
        _lock: lock,
    }
}

fn create_test_registry() -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();

    registry.register(opencode_provider::anthropic::AnthropicProvider::new(
        "test-key",
    ));
    registry.register(opencode_provider::openai::OpenAIProvider::new("test-key"));
    registry.register(opencode_provider::google::GoogleProvider::new("test-key"));
    registry.register(opencode_provider::deepseek::DeepSeekProvider::new(
        "test-key",
    ));
    registry.register(opencode_provider::mistral::MistralProvider::new("test-key"));
    registry.register(opencode_provider::groq::GroqProvider::new("test-key"));
    registry.register(opencode_provider::xai::XaiProvider::new("test-key"));
    registry.register(opencode_provider::cohere::CohereProvider::new("test-key"));
    registry.register(opencode_provider::cerebras::CerebrasProvider::new(
        "test-key",
    ));
    registry.register(opencode_provider::together::TogetherProvider::new(
        "test-key",
    ));
    registry.register(opencode_provider::perplexity::PerplexityProvider::new(
        "test-key",
    ));
    registry.register(opencode_provider::openrouter::OpenRouterProvider::new(
        "test-key",
    ));

    registry
}

#[test]
fn test_registry_lists_providers() {
    let registry = create_test_registry();
    let providers = registry.list_providers();

    assert!(!providers.is_empty(), "Registry should have providers");

    let provider_ids: Vec<&str> = providers.iter().map(|p| p.id.as_str()).collect();
    assert!(
        provider_ids.contains(&"anthropic"),
        "Should have anthropic provider"
    );
    assert!(
        provider_ids.contains(&"openai"),
        "Should have openai provider"
    );
    assert!(
        provider_ids.contains(&"google"),
        "Should have google provider"
    );
}

#[test]
fn test_registry_lists_models() {
    let registry = create_test_registry();
    let models = registry.list_models();

    assert!(!models.is_empty(), "Registry should have models");

    let model_ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert!(
        model_ids.iter().any(|id| id.contains("claude")),
        "Should have Claude models"
    );
    assert!(
        model_ids.iter().any(|id| id.contains("gpt")),
        "Should have GPT models"
    );
}

#[test]
fn test_find_model_by_id() {
    let registry = create_test_registry();

    let result = registry.find_model("claude-3-5-sonnet-20241022");
    assert!(result.is_some(), "Should find claude-3-5-sonnet model");

    let (provider_id, model) = result.unwrap();
    assert_eq!(provider_id, "anthropic");
    assert!(model.supports_vision);
    assert!(model.supports_tools);
}

#[test]
fn test_provider_metadata() {
    let registry = create_test_registry();

    let anthropic = registry.get("anthropic");
    assert!(anthropic.is_some());
    let provider = anthropic.unwrap();

    assert_eq!(provider.id(), "anthropic");
    assert_eq!(provider.name(), "Anthropic");

    let models = provider.models();
    assert!(!models.is_empty());

    let claude = provider.get_model("claude-3-5-sonnet-20241022");
    assert!(claude.is_some());
}

#[test]
fn test_chat_request_builder() {
    let request = ChatRequest::new(
        "gpt-4o",
        vec![Message::system("You are helpful"), Message::user("Hello")],
    )
    .with_temperature(0.7)
    .with_max_tokens(1000)
    .with_stream(true);

    assert_eq!(request.model, "gpt-4o");
    assert_eq!(request.messages.len(), 2);
    assert_eq!(request.temperature, Some(0.7));
    assert_eq!(request.max_tokens, Some(1000));
    assert_eq!(request.stream, Some(true));
}

#[test]
fn test_model_info_clone() {
    let model = ModelInfo {
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        provider: "test".to_string(),
        context_window: 128000,
        max_output_tokens: 4096,
        supports_vision: true,
        supports_tools: true,
        cost_per_million_input: 1.0,
        cost_per_million_output: 2.0,
    };

    let cloned = model.clone();
    assert_eq!(cloned.id, model.id);
    assert_eq!(cloned.context_window, model.context_window);
}

#[test]
fn test_all_providers_have_models() {
    let registry = create_test_registry();

    for provider in registry.list() {
        let models = provider.models();
        assert!(
            !models.is_empty(),
            "Provider {} should have models",
            provider.id()
        );
    }
}

// --- Anthropic SSE 行分割の回帰テスト -------------------------------------

const ANTHROPIC_SSE_BODY: &str = concat!(
    "event: message_start\n",
    "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":7,\"output_tokens\":0}}}\n",
    "\n",
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hello\"}}\n",
    "\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n",
    "\n",
);

/// 1 接続だけを処理するローカル HTTP モックを起動する。
///
/// ループバックへの bind が不可な環境では `SKIPPED` マーカーを出して `None` を返す。
/// 新規依存は使わず、`std::net::TcpListener` と `std::thread` のみで構成する。
/// 呼び出し側の `ensure_no_proxy()` がループバックをプロキシ対象外にするため、
/// 実行者のプロキシ設定に依存しない（ガードは終了時に元値を復元する）。
fn spawn_anthropic_sse_mock(writes: Vec<Vec<u8>>) -> Option<(String, std::thread::JoinHandle<()>)> {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("SKIPPED: loopback TCP bind unavailable: {err}");
            return None;
        }
    };
    let addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("SKIPPED: loopback TCP address unavailable: {err}");
            return None;
        }
    };

    let content_length: usize = writes.iter().map(Vec::len).sum();
    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
    );

    let handle = std::thread::spawn(move || {
        let (mut stream, _) = match listener.accept() {
            Ok(pair) => pair,
            Err(_) => return,
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut request_buf = [0u8; 4096];
        let _ = stream.read(&mut request_buf);

        if stream.write_all(headers.as_bytes()).is_err() {
            return;
        }
        for (index, payload) in writes.into_iter().enumerate() {
            if index > 0 {
                // 2 回目の write が別チャンクとして届くよう、間に短い遅延を入れる。
                std::thread::sleep(Duration::from_millis(100));
            }
            if stream.write_all(&payload).is_err() {
                return;
            }
            let _ = stream.flush();
        }
    });

    Some((format!("http://{addr}"), handle))
}

async fn collect_anthropic_events(base_url: &str) -> Vec<StreamEvent> {
    let provider = AnthropicProvider::with_config(AnthropicConfig {
        api_key: "test-key".to_string(),
        base_url: Some(base_url.to_string()),
    });
    let request =
        ChatRequest::new("claude-3-5-sonnet-20241022", vec![Message::user("hi")]).with_stream(true);

    let mut stream = provider
        .chat_stream(request)
        .await
        .expect("chat_stream should succeed");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event should not be an error"));
    }
    events
}

fn assert_expected_anthropic_events(events: &[StreamEvent]) {
    let usage = events.iter().position(|event| {
        matches!(
            event,
            StreamEvent::Usage {
                prompt_tokens: 7,
                completion_tokens: 0
            }
        )
    });
    let text = events
        .iter()
        .position(|event| matches!(event, StreamEvent::TextDelta(t) if t == "hello"));

    assert!(usage.is_some(), "Usage event missing; got {events:?}");
    assert!(
        text.is_some(),
        "TextDelta(\"hello\") missing; got {events:?}"
    );
    assert!(
        matches!(events.last(), Some(StreamEvent::Done)),
        "Done must be the last event; got {events:?}"
    );
    assert!(
        usage.unwrap() < text.unwrap(),
        "Usage must precede text; got {events:?}"
    );
}

#[tokio::test]
async fn anthropic_stream_single_chunk_events_unchanged() {
    let _no_proxy = ensure_no_proxy().await;
    let Some((base_url, server)) =
        spawn_anthropic_sse_mock(vec![ANTHROPIC_SSE_BODY.as_bytes().to_vec()])
    else {
        return; // SKIPPED マーカーはヘルパが出力済み
    };

    let events = collect_anthropic_events(&base_url).await;
    let _ = server.join();
    assert_expected_anthropic_events(&events);
}

const ANTHROPIC_SSE_BODY_JAPANESE: &str = concat!(
    "event: content_block_delta\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"こんにちは\"}}\n",
    "\n",
    "event: message_stop\n",
    "data: {\"type\":\"message_stop\"}\n",
    "\n",
);

#[tokio::test]
async fn anthropic_stream_reassembles_data_line_split_across_writes() {
    let _no_proxy = ensure_no_proxy().await;
    let split_at = ANTHROPIC_SSE_BODY
        .find("\"text\":\"hel")
        .map(|index| index + "\"text\":\"hel".len())
        .expect("fixture must contain the split marker");
    let (first_write, second_write) = ANTHROPIC_SSE_BODY.split_at(split_at);
    assert!(first_write.ends_with("\"text\":\"hel"));
    assert!(second_write.starts_with("lo\"}}"));

    let Some((base_url, server)) = spawn_anthropic_sse_mock(vec![
        first_write.as_bytes().to_vec(),
        second_write.as_bytes().to_vec(),
    ]) else {
        return; // SKIPPED マーカーはヘルパが出力済み
    };

    let events = collect_anthropic_events(&base_url).await;
    let _ = server.join();
    assert_expected_anthropic_events(&events);
}

#[tokio::test]
async fn anthropic_stream_preserves_multibyte_text_split_mid_character() {
    let _no_proxy = ensure_no_proxy().await;
    let text = "こんにちは";
    let kana_start = ANTHROPIC_SSE_BODY_JAPANESE
        .find(text)
        .expect("fixture must contain the Japanese text");
    // "こ" (E3 81 93) の先頭 1 バイト目の直後で分割する。
    let split_at = kana_start + 1;
    assert!(!ANTHROPIC_SSE_BODY_JAPANESE.is_char_boundary(split_at));
    let body_bytes = ANTHROPIC_SSE_BODY_JAPANESE.as_bytes();
    let (first_write, second_write) = body_bytes.split_at(split_at);

    let Some((base_url, server)) =
        spawn_anthropic_sse_mock(vec![first_write.to_vec(), second_write.to_vec()])
    else {
        return; // SKIPPED マーカーはヘルパが出力済み
    };

    let events = collect_anthropic_events(&base_url).await;
    let _ = server.join();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, StreamEvent::TextDelta(t) if t == text)),
        "マルチバイト文字が U+FFFD に置換されてはならない: {events:?}"
    );
    assert!(matches!(events.last(), Some(StreamEvent::Done)));
}

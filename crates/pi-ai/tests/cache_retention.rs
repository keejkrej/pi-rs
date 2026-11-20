//! Port of packages/ai/test/cache-retention.test.ts (unit ai-providers-builtin)

use std::sync::{Arc, Mutex};

use pi_ai::api::anthropic_messages::stream as stream_anthropic;
use pi_ai::api::openai_completions::stream as stream_openai_completions;
use pi_ai::api::openai_responses::stream as stream_openai_responses;
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    ApiStreamOptions, CacheRetention, ConstrainedSamplingConfig, Context, JsonSchemaStrict, Message, Model,
    ModelCompat, ModelCost, ModelInput, OpenAICompletionsCompat, OpenAIResponsesCompat, StreamOptions, SystemMessage,
    SystemMessageContent, Tool, UserMessage, UserMessageContent,
};
use pi_js::vendor::typebox::Schema;
use serde_json::{Value, json};
use serial_test::serial;

struct CacheEnvGuard {
    orig: Option<String>,
}

impl CacheEnvGuard {
    fn new() -> Self {
        let orig = pi_js::env::var("PI_CACHE_RETENTION");
        pi_js::env::remove_var("PI_CACHE_RETENTION");
        Self { orig }
    }
}

impl Drop for CacheEnvGuard {
    fn drop(&mut self) {
        match &self.orig {
            Some(v) => pi_js::env::set_var("PI_CACHE_RETENTION", v),
            None => pi_js::env::remove_var("PI_CACHE_RETENTION"),
        }
    }
}

fn sample_context() -> pi_ai::types::TranscriptContext {
    normalize_context(&Context {
        system_prompt: Some("You are a helpful assistant.".to_string()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("Hello".to_string()),
            timestamp: 1,
        })],
        tools: None,
    })
}

async fn capture_anthropic(model: &Model, opts: StreamOptions) -> Value {
    let slot: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let cap = Arc::clone(&slot);
    let mut o = opts;
    o.on_payload = Some(Arc::new(move |payload, _m| {
        let cap = Arc::clone(&cap);
        Box::pin(async move {
            *cap.lock().unwrap() = Some(payload);
            Err(pi_js::Error::msg("payload captured"))
        })
    }));
    stream_anthropic(
        model,
        &sample_context(),
        Some(ApiStreamOptions {
            base: o,
            ..Default::default()
        }),
    )
    .result()
    .await;
    slot.lock().unwrap().clone().expect("payload captured")
}

async fn capture_responses(model: &Model, opts: StreamOptions) -> Value {
    let slot: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let cap = Arc::clone(&slot);
    let mut o = opts;
    o.on_payload = Some(Arc::new(move |payload, _m| {
        let cap = Arc::clone(&cap);
        Box::pin(async move {
            *cap.lock().unwrap() = Some(payload);
            Err(pi_js::Error::msg("payload captured"))
        })
    }));
    stream_openai_responses(
        model,
        &sample_context(),
        Some(ApiStreamOptions {
            base: o,
            ..Default::default()
        }),
    )
    .result()
    .await;
    slot.lock().unwrap().clone().expect("payload captured")
}

async fn capture_completions(model: &Model, ctx: &pi_ai::types::TranscriptContext, opts: StreamOptions) -> Value {
    let slot: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let cap = Arc::clone(&slot);
    let mut o = opts;
    o.on_payload = Some(Arc::new(move |payload, _m| {
        let cap = Arc::clone(&cap);
        Box::pin(async move {
            *cap.lock().unwrap() = Some(payload);
            Err(pi_js::Error::msg("payload captured"))
        })
    }));
    stream_openai_completions(
        model,
        ctx,
        Some(ApiStreamOptions {
            base: o,
            ..Default::default()
        }),
    )
    .result()
    .await;
    slot.lock().unwrap().clone().expect("payload captured")
}

#[tokio::test]
#[serial]
async fn anthropic_cache_retention_payloads() {
    let _guard = CacheEnvGuard::new();

    let mut proxy_model = get_model("anthropic", "claude-haiku-4-5").unwrap();
    proxy_model.base_url = "https://my-proxy.example.com/v1".to_string();

    // Default TTL when PI_CACHE_RETENTION is not set on default model
    let default_payload = capture_anthropic(
        &get_model("anthropic", "claude-haiku-4-5").unwrap(),
        StreamOptions {
            api_key: Some("fake-key".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        default_payload["system"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );

    // 1h TTL when PI_CACHE_RETENTION=long on proxy baseUrl
    pi_js::env::set_var("PI_CACHE_RETENTION", "long");
    let proxy_payload = capture_anthropic(
        &proxy_model,
        StreamOptions {
            api_key: Some("fake-key".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        proxy_payload["system"][0]["cache_control"],
        json!({ "type": "ephemeral", "ttl": "1h" })
    );
    pi_js::env::remove_var("PI_CACHE_RETENTION");

    // Omit ttl when supports_long_cache_retention is false
    let mut no_long = proxy_model.clone();
    no_long.compat = Some(ModelCompat::AnthropicMessages(pi_ai::types::AnthropicMessagesCompat {
        supports_long_cache_retention: Some(false),
        ..Default::default()
    }));
    let no_long_payload = capture_anthropic(
        &no_long,
        StreamOptions {
            api_key: Some("fake-key".to_string()),
            cache_retention: Some(CacheRetention::Long),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        no_long_payload["system"][0]["cache_control"],
        json!({ "type": "ephemeral" })
    );

    // Omit cache_control when cache_retention is None
    let base_model = get_model("anthropic", "claude-haiku-4-5").unwrap();
    let none_payload = capture_anthropic(
        &base_model,
        StreamOptions {
            api_key: Some("fake-key".to_string()),
            cache_retention: Some(CacheRetention::None),
            ..Default::default()
        },
    )
    .await;
    assert!(none_payload["system"][0].get("cache_control").is_none());

    // String user message gets block cache_control
    let user_blocks = default_payload["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_array()
        .unwrap();
    assert_eq!(
        user_blocks.last().unwrap()["cache_control"],
        json!({ "type": "ephemeral" })
    );

    // Explicit long sets ttl: "1h"
    let explicit_long = capture_anthropic(
        &base_model,
        StreamOptions {
            api_key: Some("fake-key".to_string()),
            cache_retention: Some(CacheRetention::Long),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        explicit_long["system"][0]["cache_control"],
        json!({ "type": "ephemeral", "ttl": "1h" })
    );
}

#[tokio::test]
#[serial]
async fn openai_responses_cache_retention_payloads() {
    let _guard = CacheEnvGuard::new();

    for id in [
        "gpt-5.6-luna",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-6-astra",
        "gpt-6-luna",
        "gpt-6-sol",
        "gpt-6.1-terra",
    ] {
        assert!(get_model("openai", id).unwrap().prompt_cache.is_none(), "{id}");
    }

    pi_js::env::set_var("PI_CACHE_RETENTION", "long");
    let mut proxy_model = get_model("openai", "gpt-4o-mini").unwrap();
    proxy_model.base_url = "https://my-proxy.example.com/v1".to_string();
    let proxy_payload = capture_responses(
        &proxy_model,
        StreamOptions {
            api_key: Some("sk-fake-key".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(proxy_payload["prompt_cache_retention"], "24h");
    pi_js::env::remove_var("PI_CACHE_RETENTION");

    let mut no_long = get_model("openai", "gpt-4o-mini").unwrap();
    no_long.compat = Some(ModelCompat::OpenAIResponses(OpenAIResponsesCompat {
        supports_long_cache_retention: Some(false),
        ..Default::default()
    }));
    let no_long_payload = capture_responses(
        &no_long,
        StreamOptions {
            api_key: Some("sk-fake-key".to_string()),
            cache_retention: Some(CacheRetention::Long),
            session_id: Some("session-compat-false".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert!(no_long_payload.get("prompt_cache_retention").is_none());

    let gpt56 = get_model("openai", "gpt-5.6-sol").unwrap();
    let explicit = capture_responses(
        &gpt56,
        StreamOptions {
            api_key: Some("sk-fake-key".to_string()),
            cache_retention: Some(CacheRetention::None),
            session_id: Some("session-1".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert!(explicit.get("prompt_cache_key").is_none());
    assert!(explicit.get("prompt_cache_retention").is_none());
    assert_eq!(explicit["prompt_cache_options"], json!({ "mode": "explicit" }));

    let mini = get_model("openai", "gpt-4o-mini").unwrap();
    let mini_none = capture_responses(
        &mini,
        StreamOptions {
            api_key: Some("sk-fake-key".to_string()),
            cache_retention: Some(CacheRetention::None),
            session_id: Some("session-1".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert!(mini_none.get("prompt_cache_key").is_none());
    assert!(mini_none.get("prompt_cache_options").is_none());

    for (model_id, expected_retention, expected_opts) in [
        ("gpt-4o-mini", Some("24h"), None),
        ("gpt-6-astra", None, Some(json!({ "ttl": "30m" }))),
        ("gpt-6-sol", None, Some(json!({ "ttl": "30m" }))),
        ("gpt-6-luna", None, Some(json!({ "ttl": "30m" }))),
    ] {
        let model = get_model("openai", model_id).unwrap();
        let p = capture_responses(
            &model,
            StreamOptions {
                api_key: Some("sk-fake-key".to_string()),
                cache_retention: Some(CacheRetention::Long),
                session_id: Some("session-2".to_string()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(p["prompt_cache_key"], "session-2", "{model_id}");
        assert_eq!(
            p.get("prompt_cache_retention").and_then(Value::as_str),
            expected_retention,
            "{model_id}"
        );
        assert_eq!(p.get("prompt_cache_options"), expected_opts.as_ref(), "{model_id}");
    }
}

fn completions_model(compat: Option<OpenAICompletionsCompat>) -> Model {
    Model {
        r#type: None,
        id: "test-model".to_string(),
        name: "Test Model".to_string(),
        api: "openai-completions".to_string(),
        provider: "test-openai-completions".into(),
        base_url: "https://my-proxy.example.com/v1".to_string(),
        reasoning: false,
        thinking_level_map: None,
        prompt_cache: None,
        input: vec![ModelInput::Text],
        input_limits: None,
        cost: ModelCost::default(),
        context_window: 128000,
        max_tokens: 4096,
        headers: None,
        sampling_params: None,
        compat: compat.map(ModelCompat::OpenAICompletions),
        sampling_params_by_thinking_level: None,
    }
}

#[tokio::test]
#[serial]
async fn openai_completions_cache_retention_and_strict_tools() {
    let _guard = CacheEnvGuard::new();
    let ctx = sample_context();

    let p = capture_completions(
        &completions_model(None),
        &ctx,
        StreamOptions {
            api_key: Some("fake-key".to_string()),
            cache_retention: Some(CacheRetention::Long),
            session_id: Some("session-completions".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(p["prompt_cache_key"], "session-completions");
    assert_eq!(p["prompt_cache_retention"], "24h");

    let p_false = capture_completions(
        &completions_model(Some(OpenAICompletionsCompat {
            supports_long_cache_retention: Some(false),
            ..Default::default()
        })),
        &ctx,
        StreamOptions {
            api_key: Some("fake-key".to_string()),
            cache_retention: Some(CacheRetention::Long),
            session_id: Some("session-completions-false".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert!(p_false.get("prompt_cache_key").is_none());
    assert!(p_false.get("prompt_cache_retention").is_none());

    for model_id in [
        "deepseek-v4.1-flash",
        "deepseek-v4-pro",
        "kimi-k2.5",
        "kimi-k2.6",
        "minimax-m2.7",
    ] {
        let m = get_model("opencode", model_id).unwrap();
        let compat = m.compat.as_ref().unwrap().as_openai_completions().unwrap();
        assert_eq!(compat.supports_long_cache_retention, Some(false), "{model_id}");
        let op = capture_completions(
            &m,
            &ctx,
            StreamOptions {
                api_key: Some("fake-key".to_string()),
                cache_retention: Some(CacheRetention::Long),
                session_id: Some("session-opencode-long-cache-unsupported".to_string()),
                ..Default::default()
            },
        )
        .await;
        assert!(op.get("prompt_cache_key").is_none(), "{model_id}");
        assert!(op.get("prompt_cache_retention").is_none(), "{model_id}");
    }

    for model_id in ["gpt-oss-120b", "qwen-3.8-27b"] {
        let m = get_model("cerebras", model_id).unwrap();
        let compat = m.compat.as_ref().unwrap().as_openai_completions().unwrap();
        assert_eq!(compat.supports_strict_mode, None, "{model_id}");

        let tool_ctx = normalize_context(&Context {
            system_prompt: None,
            tools: None,
            messages: vec![
                Message::System(SystemMessage {
                    content: SystemMessageContent::Text("test".to_string()),
                    sections: None,
                    tools_added: Some(vec![
                        Tool {
                            name: "t1".to_string(),
                            description: "strict tool".to_string(),
                            parameters: Schema::from(json!({
                                "type": "object",
                                "properties": { "x": { "type": "string" } },
                                "required": ["x"]
                            })),
                            constrained_sampling: Some(
                                ConstrainedSamplingConfig::JsonSchema {
                                    strict: JsonSchemaStrict::Prefer,
                                }
                                .into(),
                            ),
                        },
                        Tool {
                            name: "t2".to_string(),
                            description: "non-strict tool".to_string(),
                            parameters: Schema::from(json!({
                                "type": "object",
                                "properties": { "y": { "type": "string" } },
                                "required": ["y"]
                            })),
                            constrained_sampling: None,
                        },
                    ]),
                    tools_removed: None,
                    timestamp: 0,
                }),
                Message::User(UserMessage {
                    content: UserMessageContent::Text("hello".to_string()),
                    timestamp: 1,
                }),
            ],
        });

        let cp = capture_completions(
            &m,
            &tool_ctx,
            StreamOptions {
                api_key: Some("fake-key".to_string()),
                session_id: Some("test".to_string()),
                ..Default::default()
            },
        )
        .await;
        for tool in cp["tools"].as_array().unwrap() {
            assert!(tool["function"].get("strict").is_none(), "{model_id}");
        }
    }
}

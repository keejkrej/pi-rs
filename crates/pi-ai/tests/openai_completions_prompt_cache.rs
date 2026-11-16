//! Port of packages/ai/test/openai-completions-prompt-cache.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::api::openai_completions::stream as stream_openai_completions;
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::types::{
    ApiStreamOptions, CacheRetention, Context, Message, Model, ModelCompat, OpenAICompletionsCompat, ProviderHeaders,
    SessionAffinityFormat, StreamOptions, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Request, Response};
use pi_js::time;
use serde_json::{Value, json};

fn ok_sse() -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "prompt_tokens_details": { "cached_tokens": 0 },
                "completion_tokens_details": { "reasoning_tokens": 0 },
            }
        })
    )
}

fn create_model() -> Model {
    let mut base = get_model("openai", "gpt-4o-mini").unwrap();
    base.compat = None;
    base.api = "openai-completions".to_owned();
    base
}

async fn capture_request(opts: StreamOptions, model: &Model) -> (Value, Headers) {
    let captured_payload = Arc::new(Mutex::new(None::<Value>));
    let cp_c = captured_payload.clone();
    let captured_headers = Arc::new(Mutex::new(Headers::new()));
    let ch_c = captured_headers.clone();

    let mut base = opts;
    base.api_key = Some("test-key".to_owned());
    base.on_payload = Some(Arc::new(move |params, _| {
        let cp = cp_c.clone();
        Box::pin(async move {
            *cp.lock().unwrap() = Some(params);
            Ok(None)
        })
    }));
    base.fetch = Some(Arc::new(move |req: Request| {
        let ch = ch_c.clone();
        Box::pin(async move {
            *ch.lock().unwrap() = req.headers;
            Ok(Response::new(
                200,
                Headers::from_pairs([("content-type", "text/event-stream")]),
                ok_sse(),
            ))
        })
    }));

    let ctx = normalize_context(&Context {
        system_prompt: Some("sys".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    });

    stream_openai_completions(
        model,
        &ctx,
        Some(ApiStreamOptions {
            base,
            ..Default::default()
        }),
    )
    .result()
    .await;

    (
        captured_payload.lock().unwrap().take().unwrap(),
        captured_headers.lock().unwrap().clone(),
    )
}

#[tokio::test]
async fn sets_prompt_cache_key_for_direct_openai_requests_when_caching_is_enabled() {
    let model = create_model();
    let (payload, _) = capture_request(
        StreamOptions {
            session_id: Some("session-123".to_owned()),
            ..Default::default()
        },
        &model,
    )
    .await;

    assert_eq!(payload.get("prompt_cache_key"), Some(&json!("session-123")));
    assert!(payload.get("prompt_cache_retention").is_none());
}

#[tokio::test]
async fn sets_prompt_cache_retention_to_24h_for_direct_openai_requests_when_long() {
    let model = create_model();
    let (payload, _) = capture_request(
        StreamOptions {
            cache_retention: Some(CacheRetention::Long),
            session_id: Some("session-456".to_owned()),
            ..Default::default()
        },
        &model,
    )
    .await;

    assert_eq!(payload.get("prompt_cache_key"), Some(&json!("session-456")));
    assert_eq!(payload.get("prompt_cache_retention"), Some(&json!("24h")));
}

#[tokio::test]
async fn clamps_prompt_cache_key_to_64_characters() {
    let model = create_model();
    let (payload, _) = capture_request(
        StreamOptions {
            session_id: Some("x".repeat(67)),
            ..Default::default()
        },
        &model,
    )
    .await;

    assert_eq!(payload.get("prompt_cache_key"), Some(&json!("x".repeat(64))));
}

#[tokio::test]
async fn omits_prompt_cache_fields_when_cache_retention_is_none() {
    let model = create_model();
    let (payload, _) = capture_request(
        StreamOptions {
            cache_retention: Some(CacheRetention::None),
            session_id: Some("session-789".to_owned()),
            ..Default::default()
        },
        &model,
    )
    .await;

    assert!(payload.get("prompt_cache_key").is_none());
    assert!(payload.get("prompt_cache_retention").is_none());
}

#[tokio::test]
async fn omits_prompt_cache_fields_for_non_openai_urls_without_compatible_long_retention() {
    let model = Model {
        base_url: "https://proxy.example.com/v1".to_owned(),
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            supports_long_cache_retention: Some(false),
            ..Default::default()
        })),
        ..create_model()
    };
    let (payload, _) = capture_request(
        StreamOptions {
            cache_retention: Some(CacheRetention::Long),
            session_id: Some("session-proxy".to_owned()),
            ..Default::default()
        },
        &model,
    )
    .await;

    assert!(payload.get("prompt_cache_key").is_none());
    assert!(payload.get("prompt_cache_retention").is_none());
}

#[tokio::test]
async fn uses_pi_cache_retention_for_direct_openai_requests() {
    let model = create_model();
    let mut env = IndexMap::new();
    env.insert("PI_CACHE_RETENTION".to_owned(), "long".to_owned());
    let (payload, _) = capture_request(
        StreamOptions {
            session_id: Some("session-env".to_owned()),
            env: Some(env),
            ..Default::default()
        },
        &model,
    )
    .await;

    assert_eq!(payload.get("prompt_cache_key"), Some(&json!("session-env")));
    assert_eq!(payload.get("prompt_cache_retention"), Some(&json!("24h")));
}

#[tokio::test]
async fn sends_known_and_catalog_session_affinity_headers_and_respects_overrides() {
    // 1. sendSessionAffinityHeaders enabled
    let model1 = Model {
        base_url: "https://proxy.example.com/v1".to_owned(),
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            send_session_affinity_headers: Some(true),
            ..Default::default()
        })),
        ..create_model()
    };
    let (_, h1) = capture_request(
        StreamOptions {
            session_id: Some("session-affinity".to_owned()),
            ..Default::default()
        },
        &model1,
    )
    .await;
    assert_eq!(h1.get("session_id").as_deref(), Some("session-affinity"));
    assert_eq!(h1.get("x-client-request-id").as_deref(), Some("session-affinity"));
    assert_eq!(h1.get("x-session-affinity").as_deref(), Some("session-affinity"));

    // 2. Fireworks session affinity
    for model_id in [
        "accounts/fireworks/models/glm-5p3",
        "accounts/fireworks/routers/glm-5p3-fast",
    ] {
        let fw = get_model("fireworks", model_id).unwrap();
        let (_, h_fw) = capture_request(
            StreamOptions {
                session_id: Some("fireworks-session".to_owned()),
                ..Default::default()
            },
            &fw,
        )
        .await;
        assert_eq!(h_fw.get("x-session-affinity").as_deref(), Some("fireworks-session"));
    }

    // 3. Baseten session affinity
    let baseten = get_model("baseten", "zai-org/GLM-5.2").unwrap();
    let (_, h_bt) = capture_request(
        StreamOptions {
            session_id: Some("baseten-catalog-session".to_owned()),
            ..Default::default()
        },
        &baseten,
    )
    .await;
    assert_eq!(
        h_bt.get("x-session-affinity").as_deref(),
        Some("baseten-catalog-session")
    );
    assert_eq!(
        h_bt.get("x-client-request-id").as_deref(),
        Some("baseten-catalog-session")
    );

    // 4. OpenAI no-session format
    let model_nosess = Model {
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            send_session_affinity_headers: Some(true),
            session_affinity_format: Some(SessionAffinityFormat::OpenAINosession),
            ..Default::default()
        })),
        ..create_model()
    };
    let (p_ns, h_ns) = capture_request(
        StreamOptions {
            session_id: Some("session-nosession".to_owned()),
            ..Default::default()
        },
        &model_nosess,
    )
    .await;
    assert!(p_ns.get("session_id").is_none());
    assert_eq!(p_ns.get("prompt_cache_key"), Some(&json!("session-nosession")));
    assert!(h_ns.get("session_id").is_none());
    assert_eq!(h_ns.get("x-client-request-id").as_deref(), Some("session-nosession"));
    assert_eq!(h_ns.get("x-session-affinity").as_deref(), Some("session-nosession"));
    assert!(h_ns.get("x-session-id").is_none());

    // 5. OpenRouter explicit format
    let model_or_cfg = Model {
        base_url: "https://proxy.example.com/v1".to_owned(),
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            send_session_affinity_headers: Some(true),
            session_affinity_format: Some(SessionAffinityFormat::OpenRouter),
            ..Default::default()
        })),
        ..create_model()
    };
    let (p_or, h_or) = capture_request(
        StreamOptions {
            session_id: Some("session-proxy".to_owned()),
            ..Default::default()
        },
        &model_or_cfg,
    )
    .await;
    assert!(p_or.get("session_id").is_none());
    assert!(p_or.get("prompt_cache_key").is_none());
    assert_eq!(h_or.get("x-session-id").as_deref(), Some("session-proxy"));
    assert!(h_or.get("session_id").is_none());
    assert!(h_or.get("x-client-request-id").is_none());
    assert!(h_or.get("x-session-affinity").is_none());

    // 6. OpenRouter default catalog model
    let or_auto = get_model("openrouter", "auto").unwrap();
    let (p_auto, h_auto) = capture_request(
        StreamOptions {
            session_id: Some("session-openrouter".to_owned()),
            ..Default::default()
        },
        &or_auto,
    )
    .await;
    assert!(p_auto.get("session_id").is_none());
    assert!(p_auto.get("prompt_cache_key").is_none());
    assert_eq!(h_auto.get("x-session-id").as_deref(), Some("session-openrouter"));
    assert!(h_auto.get("session_id").is_none());
    assert!(h_auto.get("x-client-request-id").is_none());
    assert!(h_auto.get("x-session-affinity").is_none());

    // 7. OpenRouter disabled
    let model_or_off = Model {
        provider: "openrouter".to_owned(),
        base_url: "https://openrouter.ai/api/v1".to_owned(),
        compat: Some(ModelCompat::OpenAICompletions(OpenAICompletionsCompat {
            send_session_affinity_headers: Some(false),
            ..Default::default()
        })),
        ..create_model()
    };
    let (p_off, h_off) = capture_request(
        StreamOptions {
            session_id: Some("session-openrouter".to_owned()),
            ..Default::default()
        },
        &model_or_off,
    )
    .await;
    assert!(p_off.get("session_id").is_none());
    assert!(p_off.get("prompt_cache_key").is_none());
    assert!(h_off.get("x-session-id").is_none());

    // 8. cacheRetention none omits session affinity headers
    let (_, h_none) = capture_request(
        StreamOptions {
            cache_retention: Some(CacheRetention::None),
            session_id: Some("session-affinity".to_owned()),
            ..Default::default()
        },
        &model1,
    )
    .await;
    assert!(h_none.get("session_id").is_none());
    assert!(h_none.get("x-client-request-id").is_none());
    assert!(h_none.get("x-session-affinity").is_none());

    // 9. explicit headers override generated headers
    let mut overrides: ProviderHeaders = IndexMap::new();
    overrides.insert("session_id".to_owned(), Some("override-session".to_owned()));
    overrides.insert("x-client-request-id".to_owned(), Some("override-request".to_owned()));
    overrides.insert("x-session-affinity".to_owned(), Some("override-affinity".to_owned()));
    let (_, h_ov) = capture_request(
        StreamOptions {
            session_id: Some("session-affinity".to_owned()),
            headers: Some(overrides),
            ..Default::default()
        },
        &model1,
    )
    .await;
    assert_eq!(h_ov.get("session_id").as_deref(), Some("override-session"));
    assert_eq!(h_ov.get("x-client-request-id").as_deref(), Some("override-request"));
    assert_eq!(h_ov.get("x-session-affinity").as_deref(), Some("override-affinity"));
}

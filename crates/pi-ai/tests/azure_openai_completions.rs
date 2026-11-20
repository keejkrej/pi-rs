//! Port of packages/ai/test/azure-openai-completions.test.ts

use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use pi_ai::compat::{get_model, normalize_context};
use pi_ai::providers::azure::azure_provider;
use pi_ai::types::{
    ApiStreamOptions, AssistantContentBlock, AssistantMessage, CacheRetention, Context, Message, Model, ProviderEnv,
    SimpleStreamOptions, StopReason, StreamOptions, SystemMessage, SystemMessageContent, TextContent, ThinkingContent,
    ThinkingLevel, Usage, UserMessage, UserMessageContent,
};
use pi_js::fetch::{Headers, Request, Response};
use pi_js::time;
use serde_json::{Value, json};

fn completions_sse() -> Response {
    let chunk = json!({
        "choices": [{ "delta": {}, "finish_reason": "stop" }],
        "usage": {
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "prompt_tokens_details": { "cached_tokens": 0 }
        }
    });
    Response::new(
        200,
        Headers::from_pairs([("content-type", "text/event-stream")]),
        format!("data: {}\n\ndata: [DONE]\n\n", pi_js::json::stringify(&chunk)),
    )
}

fn responses_sse() -> Response {
    let chunk = json!({
        "type": "response.completed",
        "response": {
            "status": "completed",
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1,
                "total_tokens": 2,
                "input_tokens_details": { "cached_tokens": 0 }
            }
        }
    });
    Response::new(
        200,
        Headers::from_pairs([("content-type", "text/event-stream")]),
        format!("data: {}\n\n", pi_js::json::stringify(&chunk)),
    )
}

fn azure_env(extra: &[(&str, &str)]) -> ProviderEnv {
    let mut env = ProviderEnv::new();
    env.insert(
        "AZURE_OPENAI_BASE_URL".to_owned(),
        "https://my-resource.services.ai.azure.com".to_owned(),
    );
    for (k, v) in extra {
        env.insert((*k).to_owned(), (*v).to_owned());
    }
    env
}

async fn run_azure_simple(
    model: &Model,
    context: &pi_ai::types::TranscriptContext,
    opts: SimpleStreamOptions,
) -> (Option<String>, Option<Value>, AssistantMessage) {
    let url = Arc::new(Mutex::new(None::<String>));
    let url_c = Arc::clone(&url);
    let body = Arc::new(Mutex::new(None::<Value>));
    let body_c = Arc::clone(&body);

    let azure = azure_provider().unwrap();
    let mut o = opts;
    o.base.api_key = Some("test-key".to_owned());
    o.base.fetch = Some(Arc::new(move |req: Request| {
        *url_c.lock().unwrap() = Some(req.url.clone());
        *body_c.lock().unwrap() = req.body_json();
        let is_resp = req.url.contains("/responses");
        Box::pin(async move { Ok(if is_resp { responses_sse() } else { completions_sse() }) })
    }));
    let msg = azure.stream_simple(model, context, Some(o)).result().await;
    (url.lock().unwrap().take(), body.lock().unwrap().take(), msg)
}

async fn run_azure(
    model: &Model,
    context: &pi_ai::types::TranscriptContext,
    opts: ApiStreamOptions,
) -> (Option<String>, Option<Value>, AssistantMessage) {
    let url = Arc::new(Mutex::new(None::<String>));
    let url_c = Arc::clone(&url);
    let body = Arc::new(Mutex::new(None::<Value>));
    let body_c = Arc::clone(&body);

    let azure = azure_provider().unwrap();
    let mut o = opts;
    o.base.api_key = Some("test-key".to_owned());
    o.base.fetch = Some(Arc::new(move |req: Request| {
        *url_c.lock().unwrap() = Some(req.url.clone());
        *body_c.lock().unwrap() = req.body_json();
        let is_resp = req.url.contains("/responses");
        Box::pin(async move { Ok(if is_resp { responses_sse() } else { completions_sse() }) })
    }));
    let msg = azure.stream(model, context, Some(o)).result().await;
    (url.lock().unwrap().take(), body.lock().unwrap().take(), msg)
}

// Regression for #9645: Azure Foundry rejects DeepSeek's thinking field and every prompt cache parameter here.
#[tokio::test]
async fn azure_deepseek_v4_pro_completions_and_endpoint_and_routing() {
    let ds = get_model("azure", "deepseek-v4-pro").unwrap();
    let ctx = normalize_context(&Context {
        system_prompt: Some("sys".to_owned()),
        messages: vec![Message::User(UserMessage {
            content: UserMessageContent::Text("hi".to_owned()),
            timestamp: time::now_ms(),
        })],
        tools: None,
    });

    // Turns thinking on with reasoning_effort instead of thinking field
    let (_, b1, _) = run_azure_simple(
        &ds,
        &ctx,
        SimpleStreamOptions {
            base: StreamOptions {
                env: Some(azure_env(&[])),
                ..Default::default()
            },
            reasoning: Some(ThinkingLevel::High),
            ..Default::default()
        },
    )
    .await;
    let b1 = b1.unwrap();
    assert_eq!(b1.get("reasoning_effort"), Some(&json!("high")));
    assert!(b1.get("thinking").is_none());

    // Clamps max -> high
    let (_, b2, _) = run_azure_simple(
        &ds,
        &ctx,
        SimpleStreamOptions {
            base: StreamOptions {
                env: Some(azure_env(&[])),
                ..Default::default()
            },
            reasoning: Some(ThinkingLevel::Max),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(b2.unwrap().get("reasoning_effort"), Some(&json!("high")));

    // Omits reasoning_effort when no thinking level requested
    let (_, b3, _) = run_azure_simple(
        &ds,
        &ctx,
        SimpleStreamOptions {
            base: StreamOptions {
                env: Some(azure_env(&[])),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    let b3 = b3.unwrap();
    assert!(b3.get("reasoning_effort").is_none());
    assert!(b3.get("thinking").is_none());

    // Omits prompt cache parameters even when PI_CACHE_RETENTION=long or cache_retention=long
    let (_, b4, _) = run_azure(
        &ds,
        &ctx,
        ApiStreamOptions {
            base: StreamOptions {
                session_id: Some("session-env".to_owned()),
                env: Some(azure_env(&[("PI_CACHE_RETENTION", "long")])),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    let b4 = b4.unwrap();
    assert!(b4.get("prompt_cache_key").is_none());
    assert!(b4.get("prompt_cache_retention").is_none());

    let (_, b5, _) = run_azure(
        &ds,
        &ctx,
        ApiStreamOptions {
            base: StreamOptions {
                session_id: Some("session-1".to_owned()),
                cache_retention: Some(CacheRetention::Long),
                env: Some(azure_env(&[])),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    let b5 = b5.unwrap();
    assert!(b5.get("prompt_cache_key").is_none());
    assert!(b5.get("prompt_cache_retention").is_none());

    // System role under reasoning_effort=low
    let (_, b6, _) = run_azure(
        &ds,
        &ctx,
        ApiStreamOptions {
            base: StreamOptions {
                env: Some(azure_env(&[])),
                ..Default::default()
            },
            ..Default::default()
        }
        .with_extra("reasoningEffort", json!("low")),
    )
    .await;
    let b6 = b6.unwrap();
    assert_eq!(b6["messages"][0]["role"], json!("system"));
    assert_eq!(b6["messages"][0]["content"], json!("sys"));

    // Mid-conversation system messages kept in place
    let resumed = normalize_context(&Context {
        system_prompt: Some("first".to_owned()),
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("hi".to_owned()),
                timestamp: time::now_ms(),
            }),
            Message::System(SystemMessage {
                content: SystemMessageContent::Text("second".to_owned()),
                sections: None,
                tools_added: None,
                tools_removed: None,
                timestamp: time::now_ms(),
            }),
            Message::User(UserMessage {
                content: UserMessageContent::Text("again".to_owned()),
                timestamp: time::now_ms(),
            }),
        ],
        tools: None,
    });
    let (_, b_mid, _) = run_azure(
        &ds,
        &resumed,
        ApiStreamOptions {
            base: StreamOptions {
                env: Some(azure_env(&[])),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    let b_mid_val = b_mid.unwrap();
    let roles: Vec<&str> = b_mid_val["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, vec!["system", "user", "system", "user"]);

    // Replays reasoning_content
    let asst = AssistantMessage {
        content: vec![
            AssistantContentBlock::Thinking(ThinkingContent {
                thinking: "internal reasoning".to_owned(),
                thinking_signature: Some("reasoning_content".to_owned()),
                redacted: None,
            }),
            AssistantContentBlock::Text(TextContent::new("answer")),
        ],
        provider: "azure".into(),
        api: "openai-completions".to_owned(),
        model: "deepseek-v4-pro".to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Stop,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: time::now_ms(),
        duration_ms: None,
    };
    let with_replay = normalize_context(&Context {
        system_prompt: Some("sys".to_owned()),
        messages: vec![
            Message::User(UserMessage {
                content: UserMessageContent::Text("first".to_owned()),
                timestamp: time::now_ms(),
            }),
            Message::Assistant(asst),
            Message::User(UserMessage {
                content: UserMessageContent::Text("second".to_owned()),
                timestamp: time::now_ms(),
            }),
        ],
        tools: None,
    });
    let (_, b_rep, _) = run_azure(
        &ds,
        &with_replay,
        ApiStreamOptions {
            base: StreamOptions {
                env: Some(azure_env(&[])),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    let replayed = b_rep.unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == json!("assistant"))
        .cloned()
        .unwrap();
    assert_eq!(replayed["reasoning_content"], json!("internal reasoning"));

    // Endpoint normalization, unconfigured endpoint error, deployment name mapping & on_payload, routing
    let (u_norm, b_id, m_id) = run_azure(
        &ds,
        &ctx,
        ApiStreamOptions {
            base: StreamOptions {
                env: Some(azure_env(&[])),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        u_norm.as_deref(),
        Some("https://my-resource.services.ai.azure.com/openai/v1/chat/completions")
    );
    assert_eq!(b_id.unwrap()["model"], json!("deepseek-v4-pro"));
    assert_eq!(m_id.model, "deepseek-v4-pro");

    // Empty base_url without AZURE_OPENAI_BASE_URL surfaces error event
    let mut ds_no_url = ds.clone();
    ds_no_url.base_url.clear();
    let (_, _, m_err) = run_azure(
        &ds_no_url,
        &ctx,
        ApiStreamOptions {
            base: StreamOptions {
                env: Some(ProviderEnv::new()),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(m_err.stop_reason, StopReason::Error);
    assert!(
        m_err
            .error_message
            .unwrap()
            .contains("Azure OpenAI base URL is required")
    );

    // Mapped deployment name via stream_simple & on_payload
    let (_, b_map, m_map) = run_azure_simple(
        &ds,
        &ctx,
        SimpleStreamOptions {
            base: StreamOptions {
                env: Some(azure_env(&[(
                    "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
                    "deepseek-v4-pro=my-deepseek",
                )])),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(b_map.unwrap()["model"], json!("my-deepseek"));
    assert_eq!(m_map.model, "deepseek-v4-pro");

    let seen_model = Arc::new(Mutex::new(None::<Value>));
    let sm_ref = Arc::clone(&seen_model);
    let (_, b_onp, _) = run_azure(
        &ds,
        &ctx,
        ApiStreamOptions {
            base: StreamOptions {
                env: Some(azure_env(&[(
                    "AZURE_OPENAI_DEPLOYMENT_NAME_MAP",
                    "deepseek-v4-pro=my-deepseek",
                )])),
                on_payload: Some(Arc::new(move |mut payload, _| {
                    *sm_ref.lock().unwrap() = payload.get("model").cloned();
                    if let Some(obj) = payload.as_object_mut() {
                        obj.insert("temperature".to_owned(), json!(0.1));
                    }
                    Box::pin(async move { Ok(Some(payload)) })
                })),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_eq!(seen_model.lock().unwrap().take(), Some(json!("my-deepseek")));
    let b_onp = b_onp.unwrap();
    assert_eq!(b_onp["model"], json!("my-deepseek"));
    assert_eq!(b_onp["temperature"], json!(0.1));

    // Routing: gpt-4o-mini -> responses, deepseek-v4-pro -> chat completions
    let resp_model = get_model("azure", "gpt-4o-mini").unwrap();
    let (u_resp, _, m_resp) = run_azure(
        &resp_model,
        &ctx,
        ApiStreamOptions {
            base: StreamOptions {
                env: Some(azure_env(&[])),
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await;
    assert_ne!(m_resp.stop_reason, StopReason::Error, "{:?}", m_resp.error_message);
    assert!(u_resp.unwrap().contains("/responses"));
    let _ = IndexMap::<String, String>::new();
}
